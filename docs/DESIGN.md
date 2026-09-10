# Design

This is a living document. Update it in the same PR that changes the design
it describes — don't let it drift from what's actually built. For the
step-by-step build order, see [`PLAN.md`](PLAN.md).

## What this is

A DLNA/UPnP-AV media server: SSDP discovery, a ContentDirectory service, and
HTTP file serving with Range support. Same scope as MiniDLNA, written in
Rust so the memory-safety bug class that produced MiniDLNA's CVEs can't
happen, with two real improvements: it picks up new files without a
restart, and its Albums/Recently Added views are computed live instead of
hardcoded and buggy. No transcoding, no casting, no accounts, no web UI.

## The core seam: protocol mechanics vs. opinion

Two things any DLNA server needs are genuinely universal: SSDP, SOAP
dispatch, Range serving, and correct DIDL-Lite/protocolInfo. Two things
people actually disagree about are how the file tree gets organized for
browsing, and whether bytes get transformed before they're served. The
codebase is structured around that split, not around "core" vs. "plugins"
in the abstract:

- **Core** (protocol mechanics): SSDP, device/SCPD XML, HTTP routing and
  Range primitives, SOAP envelope parsing and dispatch, the DIDL-Lite data
  model and protocolInfo builders, config loading, process lifecycle.
  None of this needs to know how items are grouped or whether their bytes
  are transformed.
- **Extension points**, expressed as traits with default implementations
  shipped in the same binary:
  - `ContentSource` — given a container ID, returns its children. This is
    how "Folders" vs. "Albums/Artists/Recently Added" gets built, and it's
    the seam a downstream consumer would use to organize content
    differently without touching core.
  - `ByteSource` — given a resolved item, exposes a byte stream and a
    `supports_range()` flag. The default (`PassthroughSource`) serves
    files byte-for-byte. This is where a downstream fork would plug in
    transcoding — not something this project builds.
  - `MetadataProvider` — given a file path, returns whatever title/artist/
    album metadata is available. Two implementations ship: a cheap,
    filename-only one used on every Browse, and a `lofty`-backed one
    that reads real tags once per file, at scan time (Phase 12).
  - `ArtSource` — given a resolved item path, returns its embedded cover
    art, if it has any (Phase 12). Reads fresh from disk per request,
    like `ByteSource`, so a large library's embedded art never sits in
    memory.

It's one crate with modules that mirror this boundary
(`core::{ssdp, http, didl, dispatch}`, `content::*`, `transform::*`,
`metadata::*`), not a multi-crate workspace. The trait definitions live in
`core`; implementations depend on `core`, never the reverse. Splitting into
separate published crates is deferred until an actual second consumer shows
up wanting to build a different binary against `core` — doing it
preemptively would be exactly the kind of speculative complexity this
project is trying to avoid.

## Encapsulation rule: core modules must not leak their backend library

Every `core` module wraps some specific third-party crate to do its actual
work — `hyper` for HTTP, `tokio`/`socket2` for the SSDP socket, `if-addrs`
for interface lookup, `quick-xml` for XML. Each of those was a considered
choice (see `SPEC.md`'s crate table) but not an irreversible one — the
whole reason to isolate "protocol mechanics" in `core` is so a choice like
that can change later without a ripple effect. That only holds if the
module's *public* API never requires a caller to name the backend's own
types. If it does, swapping the backend means changing every caller too,
and the module was never actually isolating anything — just relocating the
dependency.

**The test, concretely:** could someone rewrite this module's internals
around a different crate entirely, touching only files inside the module,
and have every other file in the project compile unchanged? If the answer
is no, something is leaking.

**Before marking anything `pub`:**

1. Grep for it: `grep -rn "modulename::"` across `src/`, `tests/`, and
   `fuzz/`. If nothing outside the module's own files matches, it doesn't
   need to be `pub` — `pub(crate)` (if a sibling module needs it) or
   nothing at all.
2. For whatever genuinely is consumed from outside, check its
   signature for backend types: `hyper::*`, `tokio::net::*`,
   `socket2::*`, `if_addrs::*`, `quick_xml::*`, and so on as new backends
   get added (`redb` and `symphonia` in later phases, most likely). If one
   shows up in a `pub fn`'s parameters or return type, or in a `pub`
   struct's `pub` field, that's a leak — narrow the type to something this
   crate owns (a wrapper struct, an enum, plain std types) before it ships.
3. `std` types, `Duration`, `PathBuf`, and this crate's own domain types
   (`uuid::Uuid`, `ServiceType`, `Target`) are fine to share freely — they
   aren't "how do we do the thing" implementation choices, they're shared
   vocabulary every module needs. The rule is about *backend* libraries
   specifically, not about minimizing dependencies in general.

**A fuzz target needing a pure function is not an exception.** It's tempting
to make a whole module `pub` because `fuzz/` (a separate crate — see
`fuzz/Cargo.toml`) needs to reach one parsing function inside it. Don't:
that makes everything else in the module externally reachable too, backend
types included, for the sake of one function nothing else needs. Instead,
add a one-line re-export to the `fuzz_support` module in `src/lib.rs`:

```rust
#[doc(hidden)]
pub mod fuzz_support {
    pub use crate::core::ssdp::message::parse_search_request;
    // pub use crate::core::soap::parse_body; // whenever soap_parse lands
}
```

The module it re-exports from stays `pub(crate)`. This is the pattern for
every future fuzz target (`soap_parse`, `range_parse`, `path_resolve` — see
`PLAN.md` Phases 5–6): one new line here, not a visibility change there.

**Precedent, if you want the worked example:** `core::http` wraps `hyper`.
`HttpServer::bind`/`serve`/`local_addr` take and return only `Ipv4Addr`,
`u16`, `String`, `Uuid`, `SocketAddr` — no `hyper` type anywhere. Its
`router`/`description`/`scpd` submodules (where `hyper::Method` and
`quick_xml` actually appear) are `pub(crate)`, reachable only through
`HttpServer`. `core::ssdp` follows the same shape: `Ssdp`'s public API is
backend-free, and `message`/`targets` are `pub(crate)`, with the one
legitimate external need (the `ssdp_parse` fuzz target) going through
`fuzz_support` instead of widening the module.

## Current state

Phases 1 through 3 are done. The binary parses CLI args, loads and validates
`dlna-rs.toml` (full schema, `deny_unknown_fields` everywhere, fails loudly
on a bad port/UUID/log-level/view name), and shuts down cleanly on
SIGTERM/SIGINT. On top of that, it now answers SSDP discovery: it joins
the `239.255.255.250:1900` multicast group on the configured interface,
answers `M-SEARCH` for all five identities it advertises (root device,
UUID, device type, ContentDirectory, ConnectionManager), re-announces
`ssdp:alive` on a configurable interval, and sends `ssdp:byebye` on
shutdown. `src/core/ssdp/message.rs` holds the pure parse/build functions;
`src/core/ssdp/mod.rs` is the socket plumbing around them; `src/core/net.rs`
resolves a configured interface name to its IPv4 address (needed both for
joining the right multicast interface and for building the LOCATION URL).

Verified against a real LAN, not just unit tests: `examples/ssdp_discover.rs`
sends an M-SEARCH and prints every response, `examples/ssdp_monitor.rs`
passively watches all SSDP multicast traffic. Both are checked-in tools for
repeatable manual verification, not one-off scripts — run them again
whenever SSDP behavior needs a real-world sanity check.

Phase 3 adds the other half of discoverability: a real HTTP server
(`hyper` 1.x direct — a plain `TcpListener` accept loop, no framework)
serving `/description.xml` and SCPD documents for both services. Two new
core modules: `core::device` holds `ServiceType` and the URL scheme both
SSDP and HTTP need to agree on (moved out of `core::ssdp::targets`, which
had no business owning a concept it merely consumes), and `core::http`
holds the router (`router.rs`, a pure synchronous `route()` match — same
"decision separate from execution" shape as everything else so far),
the device description builder (`description.rs`, pure like the SSDP
message builders), and static SCPD content (`scpd.rs` + `scpd/*.xml`,
`include_str!`'d — SCPD doesn't vary at runtime, so it isn't generated).

Phase 4 adds the first of the three extension-point traits:
`ContentSource`, with its first implementation, `content::folder::FolderMirror`,
a 1:1 filesystem mirror. It's backed by two new top-level modules —
`index.rs` (the in-memory tree: `ObjectId`, `Entry`/`Container`/`Item` as
the read-only view, `IndexBuilder` as the only way to construct one) and
`scanner.rs` (`walkdir`-based, builds an `Index` from the configured media
directories) — kept at the top level rather than under `content/` because
both `FolderMirror` now and `MusicLibraryView` later (Phase 8) read the
*same* index, just presenting different tree shapes over it; it isn't
either source's private state.

Phase 5 wires that trait up to real SOAP requests: `core::soap` (envelope
parsing/building), `core::dispatch` (routes an action to a handler,
returns a response or fault envelope), and `core::didl` (renders
`index::Entry` as DIDL-Lite XML, plus the per-format `protocolInfo` table).
`ContentSource` itself moved to `core::content_source` — Phase 4 had
followed the spec's file-tree sketch and defined it in `content/mod.rs`,
which turned out to directly contradict the spec's own prose ("the traits
[live in] core... implementations depend on core, never the reverse").
That contradiction was harmless while nothing in `core` needed the trait
yet; `core::dispatch` needing to call through it is what would have made
the wrong dependency direction permanent, so it got fixed first. `Http-
Server::bind` takes `impl ContentSource` (generic, not `Arc<dyn
ContentSource>`) specifically so that `main.rs` and `tests/integration.rs`
— separate crates from the library, since a package with both a lib and a
bin target compiles them separately — never need to import `ContentSource`
at all; they just pass a `FolderMirror` and the type-erasure to `Arc<dyn
ContentSource>` happens inside `bind`. That's what let `content_source`
(and `didl`, `dispatch`, `soap`, and `device`, once the same grep-for-
external-use check was actually applied to them) stay `pub(crate)` instead
of `pub` — see the encapsulation guidance below.

Phase 6 adds the second extension-point trait, `ByteSource`
(`core::byte_source`), with `PassthroughSource` (`transform::passthrough`)
as its implementation, and wires real file-serving into `core::http`:
`/item/{id}` GET/HEAD, with `Range` support (`core::http::range`).
`HttpServer::bind` takes `impl ByteSource` the same way it already took
`impl ContentSource` — same reasoning, same payoff (`core::byte_source`
stays `pub(crate)`). One real adaptation from the spec's literal design:
the "path resolution" security concern (§7's `&str -> Result<PathBuf,
Error>`, reject `..`/traversal) assumes a URL that embeds a real path
fragment. This project's `/item/{id}` scheme (decided in Phase 5) is an
opaque ID that only ever drives an index lookup — no attacker string is
ever concatenated into a filesystem path, so that bug class has no way in
here. The actual residual risk is `follow_symlinks` letting something
inside the configured root resolve to a target outside it, which is a
filesystem-verification question, not a string-parsing one — see
`docs/THREAT_MODEL.md` and `docs/PLAN.md` Phase 6 for the full reasoning
and what got built instead.

Phase 7 makes the index a *live* thing instead of a value computed once at
startup: `index::SharedIndex` (an `Arc<RwLock<Index>>` handle) and
`rescan.rs` (a sleep-then-scan loop, each scan on a blocking thread pool
task so directory walking never stalls the async runtime). The design
choice worth calling out: `SharedIndex` isn't private state inside
`FolderMirror` — it's its own type specifically so Phase 8's
`MusicLibraryView`, which the spec says "queries the same underlying
Index `FolderMirror` reads from," can hold a clone of the exact same
handle. A rescan replacing the index updates every `ContentSource`
reading through a clone of it, automatically — nothing to separately
invalidate. `main.rs` also changed shape here: it now binds HTTP/SSDP
with an empty index and starts serving immediately, and the rescan task
(spawned right after) does the real populate — SSDP/`description.xml`
answer right away rather than waiting on a first scan that could be slow
for a large library.

Phase 8 adds the third extension-point trait, `MetadataProvider`
(`core::metadata_provider`), narrower than the spec's own description of
it. SPEC.md §10 lists title, artist, album, track, and genre as this
trait's job. But §5.1 already defines how MVP groups albums and artists:
by folder structure, read straight off `SharedIndex`. Giving both
`MetadataProvider` and the folder heuristic a claim on artist and album
would let them disagree on a real, messy library, so `MetadataProvider`
now does only what §5.1 leaves undone — pull a track number and a clean
title out of a filename (`metadata::filename::FilenameMetadata`). A
future tag-reading provider can add real artist/album/genre fields; the
`Metadata` struct already leaves room.

`content::music_library::MusicLibraryView` is the grouping logic itself.
It holds a `SharedIndex` clone, the same one `FolderMirror` reads, and
walks `Container.parent_id` chains to find albums (a track's direct
parent) and artists (that parent's own parent) — no separate index, no
extra scan. `content::composite::CompositeContentSource` then mounts one
`ContentSource` per configured view under its own ID prefix, joined with
`$` (`"albums$42"`) rather than MiniDLNA's `/`, since the router already
rejects `/` inside an object ID and a different separator needed no
router change. An ID with no matching prefix, or a known prefix paired
with an ID its mount doesn't recognize, returns `None` — the same
fail-closed contract every `ContentSource` here keeps.

A property test (an arbitrary real directory tree, scanned for real,
browsed through Albums and Artists) caught three real bugs before this
phase shipped: a top-level entry reporting its real folder parent
instead of the view's own root, a folder that holds both a track and a
sub-folder leaking that sub-folder into the track listing, and a folder
that qualifies as both an album and an artist getting shown — and
answered for — in both roles at once. `docs/PLAN.md`'s Phase 8 section
has the full account of each bug and its fix; worth reading as a
worked example of what this kind of property test is actually for.

Phase 9 changes no design. It tests one. Nothing in `src/` gained a new
module or a new trait; the work was proving three things that were true by
construction but never actually checked: all four fuzz targets survive a
sustained 120-second run, `dlna-rs` itself carries zero unsafe code (an
independent `cargo geiger` check, not just the compiler's
`#![forbid(unsafe_code)]`), and a panic in one connection's task really
does leave every other connection untouched. A real test now proves that
last claim; before this phase, only a doc comment did.
`systemd/dlna-rs.service` ships the deployment half: every sandboxing
directive in it was verified against a real running instance, not written
from documentation alone, and a real negative control
(`RestrictAddressFamilies` without `AF_NETLINK`) confirmed that dropping
the wrong permission fails loudly at startup instead of quietly breaking
interface resolution. See `docs/PLAN.md`'s Phase 9 section for the full
verification log.

Phase 11 is real-client sign-off, not new design either. A third-party
control point, the DTS Play-Fi app, found `dlna-rs` over SSDP, browsed its
library, and played back both FLAC and MP3 with working seek, including a
24-bit FLAC. That closes the loop this whole document has described in the
abstract: a real DLNA client, not this project's own test tooling,
exercising discovery, ContentDirectory, and Range serving together. See
`docs/PLAN.md`'s Phase 11 section for the details.

Phase 12 is the first real post-MVP addition: real tag reading and
embedded cover art, using `lofty` rather than the `symphonia` the
private planning notes originally named (see `docs/PLAN.md`'s Phase 12
section for the comparison and why it changed). It adds the fourth
extension point, `ArtSource`, and gives `MetadataProvider` a second real
implementation, `metadata::tags::TagMetadata`, alongside the existing
filename-only one. The two live side by side on purpose: the cheap one
still runs on every Browse to sort tracks, and the `lofty`-backed one
runs once per file, at scan time, with its result cached on `index::Item`.
`lofty` itself is named in exactly one file. Nothing in `core`, the
scanner's call shape, or any consumer would need to change if that
backend were ever swapped for something else.

See [`PLAN.md`](PLAN.md) for what's next and why the phases are ordered the
way they are.

## Two things not in the original spec

- **`[ssdp] notify_interval`** — the spec called for a configurable NOTIFY
  interval but never added the config table for it. Added in Phase 2;
  `CACHE-CONTROL: max-age` on every advertisement is derived as twice this
  value.
- **`if-addrs` and `socket2` dependencies** — needed for interface-name-to-IP
  resolution and for `SO_REUSEADDR`/`SO_REUSEPORT` (so other UPnP software
  on the same host can share port 1900), neither of which the original
  crate table anticipated. Both are small, pure-Rust-facing wrappers; the
  unsafe FFI they do internally doesn't touch `forbid(unsafe_code)` in our
  own crates.
- **SCPD declares only implemented actions, not the full UPnP-optional
  set.** The Phase 3 plan originally called for spec-complete SCPD (every
  action UPnP allows for `ContentDirectory`/`ConnectionManager`, whether or
  not this server implements it). Real minimal servers don't do that —
  MiniDLNA's own SCPD lists only what it actually supports. Followed that
  precedent instead: SCPD as an honest capability list.
- **`ByteSource` reads are buffered, not streamed**, and boxed by hand
  instead of using the `async-trait` crate. Neither was in the spec's
  crate table (which didn't anticipate needing either choice) — both are
  Phase 6 calls made for files this size (see `docs/PLAN.md` Phase 6).

## Encapsulation, one more round

Applying the same test from above to everything Phase 6 added:
`core::byte_source` (the trait), `core::http::range` (parsing), and
`transform::passthrough` were checked against `main.rs`/`tests/`/`fuzz/`
the same way `content_source`/`didl`/`dispatch`/`soap` were last time.
Only `transform::passthrough::PassthroughSource` (a concrete type
`main.rs` and `tests/integration.rs` construct directly, same as
`FolderMirror`) and `core::http::range::parse`/
`core::http::router::parse_item_path` (needed by their fuzz targets, via
`fuzz_support`) cross the boundary; `core::byte_source` itself stays
`pub(crate)`.

Phase 8 added four modules; the same grep check gave four different
answers. `core::metadata_provider` (the trait) stays `pub(crate)` —
nothing outside `core` names it directly, only the concrete
`FilenameMetadata` type. `content::composite` is `pub` — `main.rs` and
`tests/integration.rs` both construct `CompositeContentSource` directly,
the same real-external-use test every other `pub` module here passes.
`content::music_library` and `metadata::filename` are `pub(crate)`:
unlike `FolderMirror` or `PassthroughSource`, `MusicLibraryView` and
`FilenameMetadata` are never constructed outside `src/` — only
`content::composite`/`metadata::tags` (sibling modules, same crate)
reach them, so the grep check says `pub(crate)`, not `pub`, and that's
what they are. (The top-level `metadata` module itself became `pub` in Phase 12, once
something outside `src/` genuinely needed one of its submodules; see
below.) Worth narrowing later if a real external caller
shows up; not worth guessing at now.

Phase 12 added two more modules. `metadata::tags` (holding
`TagMetadata`) is `pub`: `main.rs` and `tests/integration.rs` both
construct it directly, to pass as `HttpServer::bind`'s `art_source`
argument, the same real-external-use test everything else here passes.
That's also why the top-level `metadata` module (`src/lib.rs`) went
from `pub(crate)` to `pub` this phase. A module can never be more open
than its parent, and `main.rs` now genuinely needs to reach through it.
`core::art_source` (the `ArtSource` trait) stays `pub(crate)`, same
reasoning as `core::byte_source`: `HttpServer::bind` takes `impl
ArtSource + 'static`, so `main.rs` and `tests/integration.rs` only ever
need to name the concrete `TagMetadata` type, never the trait itself.

## Config

One TOML file, one schema, no partial/silent failures. Every table uses
`#[serde(deny_unknown_fields)]`, so a typo'd key fails startup instead of
getting silently ignored. Where serde's own error messages are already
good enough (an unknown `library.views` entry, for instance, gets
`unknown variant 'x', expected one of ...` for free from the enum
deserializer), we don't duplicate that logic — validation in
`Config::validate()` is reserved for checks serde can't express, like
"pick a port above 1024" or "media.directories can't be empty."
