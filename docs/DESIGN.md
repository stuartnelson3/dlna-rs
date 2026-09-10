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

Phase 1 is done: the binary parses CLI args, loads and validates
`dlna-rs.toml` (full schema, `deny_unknown_fields` everywhere, fails loudly
on a bad port/UUID/log-level/view name), logs what it loaded, and shuts
down cleanly on SIGTERM/SIGINT. None of the `ContentSource`/`ByteSource`/
`MetadataProvider` traits exist yet — that starts in Phase 4.

See [`PLAN.md`](PLAN.md) for what's next and why the phases are ordered the
way they are.

## Config

One TOML file, one schema, no partial/silent failures. Every table uses
`#[serde(deny_unknown_fields)]`, so a typo'd key fails startup instead of
getting silently ignored. Where serde's own error messages are already
good enough (an unknown `library.views` entry, for instance, gets
`unknown variant 'x', expected one of ...` for free from the enum
deserializer), we don't duplicate that logic — validation in
`Config::validate()` is reserved for checks serde can't express, like
"pick a port above 1024" or "media.directories can't be empty."
