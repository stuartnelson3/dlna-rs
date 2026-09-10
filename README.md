# dlna-rs

A DLNA/UPnP-AV media server for the LAN, written in Rust.

The pitch is basically MiniDLNA: point it at a folder of music, it shows up
on your TV and speakers. The reasons to write a new one instead of just
running MiniDLNA are that MiniDLNA is unmaintained C code with a history of
the usual memory-safety CVEs, and its Albums/Recently Added views are
hardcoded and buggy (Recently Added silently caps out and stops updating
past 50 items). This rewrites the same narrow scope in Rust and fixes those
two things. It's not trying to be Plex or Jellyfin — no transcoding, no
casting, no web UI, no accounts.

## Status

Nothing's built yet. The design is done; see [`docs/PLAN.md`](docs/PLAN.md)
for the phased plan this repo is tracking instead of GitHub issues.

## What it does

- Discovery over SSDP, a ContentDirectory service, and HTTP file serving
  with Range support — enough for TVs, Sonos-class speakers, VLC, and
  BubbleUPnP to find and play files.
- Rescans the media directory on a timer, so new files show up without a
  restart.
- Real Albums/Artists/Recently Added views, computed live off the index
  instead of a stale cache, with configurable counts.

## What it won't do

No transcoding, no codec work, no Chromecast/AirPlay/HLS, no accounts. LAN
exposure only — this isn't meant to be port-forwarded to the internet.

## Building

Nothing to build yet — check back after Phase 0 in `docs/PLAN.md`.

## Docs

- [`docs/PLAN.md`](docs/PLAN.md) — phased implementation plan.
- `docs/DESIGN.md` / `docs/THREAT_MODEL.md` — added in Phase 0, kept
  up to date as the design evolves.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), your
choice — the standard for Rust projects.
