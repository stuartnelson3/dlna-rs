# dlna-rs

A DLNA/UPnP-AV media server for the LAN, written in Rust.

The pitch is basically MiniDLNA: point it at a folder of music, and it
shows up on your TV and speakers. MiniDLNA has two problems this project
fixes. It is unmaintained C code with a history of memory-safety CVEs.
Its Albums and Recently Added views are hardcoded and buggy: Recently
Added silently stops updating past 50 items. This project rewrites that
narrow scope in Rust and fixes those two problems. It is not trying to
be Plex or Jellyfin.

## Status

Phases 0 through 8 are done. Everything below works against a real
running instance. What is left is hardening and sign-off, not new
features. See [`docs/PLAN.md`](docs/PLAN.md) for the phased plan this
repo tracks instead of GitHub issues.

## What it does

- Discovery over SSDP, a ContentDirectory service, and HTTP file serving
  with Range support. That is enough for TVs, Sonos-class speakers, VLC,
  and BubbleUPnP to find and play files.
- Rescans the media directory on a timer, so new files show up without a
  restart.
- Real Albums, Artists, and Recently Added views, computed live off the
  index instead of a stale cache, with configurable counts.

## What it does not do

No transcoding, no codec work, no Chromecast, no AirPlay, no HLS, no
accounts. This is LAN-only. It is not meant to be port-forwarded to the
internet.

## Building

```
cargo build --release
cp examples/dlna-rs.example.toml dlna-rs.toml   # edit paths/interface for your setup
./target/release/dlna-rs --config dlna-rs.toml
```

That binary links against the system's glibc. A static build has no such
dependency, so it runs unmodified on another machine, like a NAS or an
old distro. Build against musl instead:

```
sudo pacman -S musl   # musl-tools on Debian/Ubuntu
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

No dependency here needs OpenSSL or another C library, so the musl build
needs no extra `RUSTFLAGS`.

`examples/ssdp_discover.rs` and `examples/ssdp_monitor.rs` are small
standalone tools for poking at SSDP traffic on your LAN while developing.
`examples/verify_item_playback.rs` and `examples/verify_music_library.rs`
do the same for browsing and playback against a real running instance.
Run any of them with `cargo run --example <name>`.

## Docs

- [`docs/PLAN.md`](docs/PLAN.md): the phased implementation plan.
- `docs/DESIGN.md` and `docs/THREAT_MODEL.md`: added in Phase 0, and kept
  up to date as the design evolves.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
your choice. That is the standard for Rust projects.
