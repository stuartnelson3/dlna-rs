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

Config loading and SSDP discovery work: it joins the LAN multicast group,
answers `M-SEARCH`, and re-announces itself on a timer. It's discoverable
but not yet playable — no ContentDirectory service or file serving yet.
See [`docs/PLAN.md`](docs/PLAN.md) for the phased plan this repo is
tracking instead of GitHub issues.

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

```
cargo build --release
cp examples/dlna-rs.example.toml dlna-rs.toml   # edit paths/interface for your setup
./target/release/dlna-rs --config dlna-rs.toml
```

`examples/ssdp_discover.rs` and `examples/ssdp_monitor.rs` are small
standalone tools for poking at SSDP traffic on your LAN while developing —
run them with `cargo run --example ssdp_discover`.

## Docs

- [`docs/PLAN.md`](docs/PLAN.md) — phased implementation plan.
- `docs/DESIGN.md` / `docs/THREAT_MODEL.md` — added in Phase 0, kept
  up to date as the design evolves.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), your
choice — the standard for Rust projects.
