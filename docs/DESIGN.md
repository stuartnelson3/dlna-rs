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

None of the `ContentSource`/`ByteSource`/`MetadataProvider` traits exist
yet — that starts in Phase 4.

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
