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
    album metadata is available. MVP ships filename/folder-heuristic
    parsing only; no binary tag parsing (see `THREAT_MODEL.md` for why).

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
`ContentSource`, defined in `content/mod.rs` with its first implementation,
`content::folder::FolderMirror`, a 1:1 filesystem mirror. It's backed by
two new top-level modules — `index.rs` (the in-memory tree: `ObjectId`,
`Entry`/`Container`/`Item` as the read-only view, `IndexBuilder` as the
only way to construct one) and `scanner.rs` (`walkdir`-based, builds an
`Index` from the configured media directories) — kept at the top level
rather than under `content/` because both `FolderMirror` now and
`MusicLibraryView` later (Phase 8) read the *same* index, just presenting
different tree shapes over it; it isn't either source's private state.
`ByteSource` and `MetadataProvider` still don't exist — Phases 6 and 8.

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

## Config

One TOML file, one schema, no partial/silent failures. Every table uses
`#[serde(deny_unknown_fields)]`, so a typo'd key fails startup instead of
getting silently ignored. Where serde's own error messages are already
good enough (an unknown `library.views` entry, for instance, gets
`unknown variant 'x', expected one of ...` for free from the enum
deserializer), we don't duplicate that logic — validation in
`Config::validate()` is reserved for checks serde can't express, like
"pick a port above 1024" or "media.directories can't be empty."
