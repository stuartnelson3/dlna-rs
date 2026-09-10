# Implementation plan

This is the working plan for building `dlna-rs`, tracked here instead of
GitHub issues so the plan and the code stay in the same history. Each phase
has a goal, concrete tasks, and an exit criterion — a check you can actually
run, not a feeling. Work the phases roughly in order; a later phase can start
early if it's genuinely unblocked, but don't call a phase done until its exit
criterion passes.

Check off tasks as you go (`[x]`). When a phase's design changes during
implementation, edit this file in the same PR — don't let it drift from what
actually got built.

Design rationale for each decision lives in the project's private planning
notes and, once Phase 0 lands, in `docs/DESIGN.md` and
`docs/THREAT_MODEL.md`. This file tracks *what to build next*, not *why*.

---

## Phase 0 — Scaffolding

**Goal:** a repo that builds, lints, and has CI, before any protocol code
exists.

- [x] `cargo init` as a single binary crate named `dlna-rs`.
- [x] Pin an MSRV via `rust-version` in `Cargo.toml` (1.85, for edition 2024).
- [x] Pick a license (dual `MIT OR Apache-2.0`) and add the `LICENSE` file(s).
- [x] `deny.toml`: license allowlist (MIT/Apache-2.0/BSD-family), crates.io
      as the only allowed source. Validated locally with `cargo-deny 0.20.2`
      (`advisories ok, bans ok, licenses ok, sources ok`).
- [x] `rustfmt.toml` / clippy config if defaults need overriding. Defaults
      are fine — nothing to override. The one real fight was clippy's
      `dead_code` lint firing on config fields that are only read via
      `Debug` right now; those got scoped `#[allow(dead_code)]` with a
      comment naming the phase that consumes each one, instead of loosening
      `-D warnings` project-wide.
- [x] GitHub Actions workflow: build, test (stable + MSRV), clippy
      (`-D warnings`), fmt check, `cargo audit`, `cargo deny check` on every
      PR (`.github/workflows/ci.yml`); a weekly `cargo audit` re-run
      (`.github/workflows/scheduled.yml`). Fuzz smoke tests are deliberately
      not wired up yet — there's nothing in `fuzz/fuzz_targets/` for them to
      run until Phase 2.
- [x] Seed `docs/DESIGN.md` and `docs/THREAT_MODEL.md` as living documents.
- [x] Push the initial commit to `origin`.

**Exit criterion:** a PR against `main` runs the full CI workflow and
passes. (Met against the actual Phase 1 code, not an empty `main.rs` — by
the time Phase 0 finished, Phase 1 already existed.)

---

## Phase 1 — Config and process skeleton

**Goal:** the binary starts, loads and validates config, and shuts down
cleanly.

- [x] `config.rs`: `serde` + `toml`, `deny_unknown_fields` at every level, a
      typo'd `library.views` entry fails startup with a message that lists
      the valid options.
- [x] CLI args (`lexopt`): config path override, `--version`.
- [x] Pick `log`+`env_logger` or `tracing` and wire it up. (Went with
      `log`+`env_logger`: no near-term need for structured metrics, and it
      keeps the dependency tree smaller — see the open question this
      resolves in the spec.)
- [x] SIGTERM handling: clean shutdown path (SSDP `ssdp:byebye` plugs into
      this in Phase 2).

**Exit criterion:** `dlna-rs --config path.toml` loads and validates a real
config file, logs the parsed result, and exits 0 on SIGTERM.

---

## Phase 2 — SSDP discovery

**Goal:** the server is discoverable on the LAN.

- [x] Join multicast on `239.255.255.250:1900` (`src/core/ssdp/mod.rs`,
      `Ssdp::bind`). Binds with `SO_REUSEADDR`/`SO_REUSEPORT` via `socket2`
      — port 1900 is a shared well-known port, and other UPnP software on
      the same host should be able to bind it too.
- [x] Respond to `M-SEARCH` for `ssdp:all`, `upnp:rootdevice`,
      `urn:schemas-upnp-org:device:MediaServer:1`, and
      `urn:schemas-upnp-org:service:ContentDirectory:1`. Went slightly
      further than the literal list: also matches on the device's own
      `uuid:...` and on `urn:schemas-upnp-org:service:ConnectionManager:1`,
      since a spec-correct root device advertises all of its own
      identities under `ssdp:all`/NOTIFY regardless of which ones get
      called out as individually-searchable — see `src/core/ssdp/targets.rs`.
- [x] Periodic `NOTIFY` (`ssdp:alive`) on a configurable interval
      (`ssdp.notify_interval` in config, new table — not in the original
      spec's config example, added here since the SSDP section always
      called for it). `CACHE-CONTROL: max-age` is derived as twice the
      interval.
- [x] `ssdp:byebye` on SIGTERM (`main.rs` calls `Ssdp::announce_byebye`
      after the shutdown signal fires).
- [x] Confirmed empirically: binding UDP 1900 and joining the multicast
      group need no elevated privilege on this host (Arch, kernel 7.2,
      unprivileged uid) — resolves spec open question #2.
- [x] `fuzz/fuzz_targets/ssdp_parse.rs`. Not just a skeleton — ran it for
      real (`cargo +nightly fuzz run ssdp_parse -- -max_total_time=30`),
      31M executions, no crashes.

**Exit criterion:** an SSDP client (`gssdp-discover` or equivalent) finds
the server and gets a correct `M-SEARCH` response. Verified against real
traffic on a live LAN with `examples/ssdp_discover.rs` and
`examples/ssdp_monitor.rs` (both checked in as reusable manual-testing
tools, not one-off scripts) — `dlna-rs` answered `ssdp:all` with exactly
its 5 advertised identities, alongside real responses from an actual
MiniDLNA instance and a hardware renderer already on the network.

---

## Phase 3 — Device description and SCPD

**Goal:** a discovered client can read the device's capabilities. Pure
`GET`-served XML only — no SOAP. (The original draft of this phase listed
"ConnectionManager stub actions" here; that's SOAP-invoked and needs the
dispatch machinery Phase 5 builds, so it moved there. ConnectionManager's
SCPD — the *description* of those actions — still belongs here.)

- [ ] Add `hyper` (1.x, features `http1`+`server`), `hyper-util` (feature
      `tokio`, for the `TokioIo` adapter — hyper 1.x dropped its own
      server loop), `http-body-util`, and `bytes`. Server is a plain
      accept loop (`TcpListener` + `hyper::server::conn::http1`) with a
      hand-written `service_fn` closure — no tower, no axum, per spec.
- [ ] `core::device`: a small module holding what both SSDP and HTTP need
      to agree on — `ServiceType` (`ContentDirectory`/`ConnectionManager`)
      and the device type string. Move `ServiceType` here out of
      `core::ssdp::targets` (it's not an SSDP concept, it's a device-model
      concept SSDP happens to consume) so SCPD routing and SSDP
      advertising can't drift apart by each defining their own copy.
      Also owns the URL scheme every service gets: `/{service}/scpd.xml`,
      `/{service}/control` (Phase 5), `/{service}/event` (unimplemented —
      not building GENA eventing for MVP; a `SUBSCRIBE` there just 404s,
      which UPnP permits).
- [ ] `core::http::description`: pure function building `/description.xml`
      from config (`friendly_name`, resolved `uuid`) plus the resolved
      interface IP/port (for the base URL) and `core::device`'s service
      list. Manufacturer/model fields are hardcoded constants (`dlna-rs`,
      repo URL, `CARGO_PKG_VERSION`) — not configurable; nobody asked for
      that knob. No `presentationURL` — there's no web UI to point at.
- [ ] `core::http::scpd`: **static** XML for `ContentDirectory` and
      `ConnectionManager` (matches the spec's own wording — SCPD content
      doesn't vary at runtime, so it's `include_str!`'d constants, not
      generated). Publish the standard, spec-complete SCPD for both
      service types, listing every action UPnP defines for them — not
      just the ones Phase 5 implements. That's what a compliant device
      does: SCPD says what's *knowable*, dispatch decides what's
      *implemented*, and unimplemented-but-declared actions get a SOAP
      fault (§5), not silence.
- [ ] `core::http::router`: `fn route(method, path) -> Route` as a plain,
      synchronous, unit-testable match — `Route::DeviceDescription`,
      `Route::Scpd(ServiceType)`, `Route::NotFound`. The async handler is
      a thin `match` on top of this pure decision. Sets the shape Phases 5
      and 6 extend (more routes, not a different pattern).
- [ ] Wire the HTTP server into `main.rs`: bind on the resolved interface
      IP (not `0.0.0.0` — matches the LAN-only, one-configured-interface
      model), spawn alongside the SSDP tasks, same abort-on-shutdown
      treatment.
- [ ] `tests/integration.rs`: add `reqwest` as a dev-dependency (per the
      spec's testing strategy), bind the server on an ephemeral
      `127.0.0.1` port, `GET` all three URLs, assert `200`, the right
      `Content-Type`, and that the body parses as XML.

**Exit criterion:** `curl` against `/description.xml` and both SCPD URLs
returns well-formed XML matching the UPnP device/service schema —
confirmed both by the integration test and by hand against a running
instance.

---

## Phase 4 — Index, scanner, and `FolderMirror`

**Goal:** the server has an in-memory picture of the media directory and can
serve it as a 1:1 tree.

- [ ] `index.rs`: in-memory index (`HashMap`/`Vec`-based).
- [ ] `scanner.rs`: `walkdir`-based walk, `exclude_patterns` support.
- [ ] `content::folder::FolderMirror` implementing `trait ContentSource`.
- [ ] Design the object-ID namespace scheme (prefix per `ContentSource`, per
      MiniDLNA's `1$FF0` pattern).
- [ ] Property test: an arbitrary directory tree never panics the scanner
      and never produces a broken parent/child link.

**Exit criterion:** the property test passes, and the index matches a fixture
directory tree exactly after a scan.

---

## Phase 5 — ContentDirectory SOAP dispatch and DIDL-Lite

**Goal:** a real client can Browse the tree and get correct XML back.

- [ ] SOAP envelope parsing with `quick-xml`, bounded body size (reject
      oversized bodies before parsing).
- [ ] `dispatch.rs`: routes ContentDirectory's `Browse`,
      `GetSearchCapabilities`, `GetSortCapabilities`, `GetSystemUpdateID`,
      plus ConnectionManager's `GetProtocolInfo`,
      `GetCurrentConnectionIDs`, `GetCurrentConnectionInfo` (moved from
      Phase 3 — these needed this same dispatch mechanism, not a
      one-off); a proper SOAP fault (`Invalid Action`) for everything
      else on both services.
- [ ] `didl.rs`: `MediaContainer`/`MediaItem` model, DIDL-Lite XML
      generation, `protocolInfo` builder.
- [ ] Golden-file tests for `protocolInfo`/DIDL-Lite per format: FLAC, MP3,
      MP4/AAC, MKV, JPEG.
- [ ] `fuzz/fuzz_targets/soap_parse.rs`.

**Exit criterion:** `BrowseDirectChildren` and `BrowseMetadata` against
`FolderMirror` on a fixture tree return DIDL-Lite matching the golden files.

---

## Phase 6 — HTTP file serving and Range

**Goal:** a client can actually play a file, including seeking.

- [ ] Path resolution as a pure function (`&str -> Result<PathBuf, Error>`):
      reject `..`, encoded traversal, and out-of-root symlinks after
      canonicalization.
- [ ] Range header parsing (`http-range-header` or hand-rolled): single
      range only, checked arithmetic, 416 for malformed or multi-range
      requests.
- [ ] `trait ByteSource` + `PassthroughSource` (direct byte-offset seeks, no
      transformation).
- [ ] Correct response headers: `Content-Type`, `Content-Length`,
      `Accept-Ranges: bytes`, `contentFeatures.dlna.org`,
      `transferMode.dlna.org`.
- [ ] `fuzz/fuzz_targets/range_parse.rs` and
      `fuzz/fuzz_targets/path_resolve.rs`.
- [ ] Property tests: path resolution never escapes the media root; Range
      parsing never panics and never produces an invalid `Content-Range`.

**Exit criterion:** `curl` with and without `Range` headers streams a fixture
file correctly, including a mid-file seek.

---

## Phase 7 — Rescan timer

**Goal:** new files show up without a restart.

- [ ] `tokio::time::interval` loop: re-walk configured directories on
      `rescan.interval`, diff against the index, update in place.
- [ ] `rescan.on_startup` config option.
- [ ] Rescan invalidates/recomputes the live-computed views from Phase 8
      (once that phase exists) rather than leaving them stale.

**Exit criterion:** a file added to the media directory appears in Browse
results after one rescan interval, with no restart.

---

## Phase 8 — Music library views

**Goal:** Albums, Artists, and Recently Added — the actual reason this
project exists instead of just running MiniDLNA.

- [ ] `trait MetadataProvider` + `FilenameMetadata` (folder-name-as-album,
      parent-folder-as-artist, `"NN - Title"` prefix parsing).
- [ ] `content::music_library::MusicLibraryView`: All Songs, Albums,
      Artists, Recently Added Songs, Recently Added Albums.
- [ ] `CompositeContentSource` mounting the views listed in
      `library.views`, each under its own ID-namespace prefix.
- [ ] Recently Added computed live from the index on each Browse call (or
      cheaply invalidated on rescan) — never a stale cached container.
- [ ] Config: `library.views`, `library.recently_added` (`songs_count`,
      `albums_count`, `max_age_days`).
- [ ] Property test: an arbitrary directory tree never produces a panic or
      a broken container in the Albums/Artists grouping.

**Exit criterion:** Browsing the root shows exactly the configured views;
adding more than 50 items doesn't break Recently Added (the MiniDLNA bug
this feature exists to avoid).

---

## Phase 9 — Security hardening pass

**Goal:** the three security-critical paths (path resolution, Range parsing,
SSDP/SOAP parsing) hold up under adversarial input, and the process itself
is hardened.

- [ ] All four fuzz targets run crash-free for a sustained local run (not
      just the CI smoke duration).
- [ ] `cargo geiger` baseline report reviewed.
- [ ] systemd unit with the hardening directives from the threat model
      (`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`,
      `PrivateTmp`, etc.).
- [ ] Resolve `DynamicUser=true` vs. a static service user, specifically
      against an NFS-mounted media directory.
- [ ] Confirm the release profile has `panic = "unwind"`, and that a panic
      in one connection task doesn't take down the process.

**Exit criterion:** fuzz targets run 60–120 seconds crash-free locally; the
systemd unit is reviewed against the threat model line by line.

---

## Phase 10 — CI/CD completion

**Goal:** the CI from Phase 0 grows into the full pipeline, and releases are
automated.

- [ ] PR/push CI: build, test, clippy, fmt, `cargo audit`, `cargo deny`,
      60–120s fuzz smoke on all four targets.
- [ ] Scheduled weekly job: `cargo audit` re-run, longer fuzz run.
- [ ] Release workflow: cross-compiled `x86_64-unknown-linux-musl` and
      `aarch64-unknown-linux-musl` static binaries, `cargo-geiger` report
      attached, binary size check against a 15MB budget.
- [ ] Dependabot config for `Cargo.toml`/`Cargo.lock`.

**Exit criterion:** a tagged release produces both static binaries and an
attached geiger report with no manual steps.

---

## Phase 11 — Real-client acceptance and MVP sign-off

**Goal:** confirm this actually works on real devices, then call it done.

- [ ] Test against at least one real hardware renderer and one software
      client (VLC, BubbleUPnP).
- [ ] Verify seeking works during playback, not just that the file opens.
- [ ] Finalize `docs/DESIGN.md` and `docs/THREAT_MODEL.md`.
- [ ] Fill in the README's Building and License sections for real.

**Exit criterion:** discoverable and playable, seek included, on real
hardware and one software client; clean `cargo audit`/`cargo deny`; zero
`unsafe` in first-party code; fuzz targets crash-free for the scheduled
duration. This is MVP v0.1.

---

## After MVP

Not phased yet — pick these up only as real need shows up, per the
project's non-goals: client-specific quirk handling, a persisted index
(`redb`) if startup time on a large library becomes a real problem,
`symphonia`-backed tag metadata and Genre browsing, Various-Artists
handling, inotify-based instant rescan.
