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

- [x] Added `hyper` (1.x, `http1`+`server`), `hyper-util` (`tokio`+`server`+
      `http1`, for the `TokioIo` adapter — hyper 1.x dropped its own server
      loop), `http-body-util`, `bytes`, and `quick-xml` (pulled forward
      from Phase 5 — needed now to validate the static SCPD files at
      test-time, and it'll be needed for SOAP/DIDL-Lite regardless).
      Server is a plain accept loop (`TcpListener` + `hyper::server::conn::http1`)
      with a hand-written `service_fn` closure — no tower, no axum.
- [x] `core::device` (`src/core/device.rs`): `ServiceType` and the device
      type string, moved out of `core::ssdp::targets` so SSDP advertising
      and SCPD routing share one definition instead of two that could
      drift apart. Owns the URL scheme: `/{service}/scpd.xml`,
      `/{service}/control` (Phase 5), `/{service}/event` (unimplemented —
      no GENA eventing for MVP; a `SUBSCRIBE` there just 404s via the
      router's catch-all, which UPnP permits for a service that doesn't
      support eventing).
- [x] `core::http::description` (`src/core/http/description.rs`): pure
      function building `/description.xml` from config (`friendly_name`,
      resolved `uuid`) plus the resolved interface IP/port and
      `core::device`'s service list. Manufacturer/model fields are
      hardcoded constants — not configurable, nobody asked for that knob.
      No `presentationURL` — no web UI to point at. XML-escapes
      `friendly_name` (it's free-text from config) via `quick_xml::escape`.
- [x] `core::http::scpd` (`src/core/http/scpd.rs` + `scpd/*.xml`): static
      XML for `ContentDirectory` and `ConnectionManager`, `include_str!`'d.
      **Correction from the plan as originally written**: declares only
      the actions actually implemented (Browse/GetSearchCapabilities/
      GetSortCapabilities/GetSystemUpdateID; GetProtocolInfo/
      GetCurrentConnectionIDs/GetCurrentConnectionInfo), not the full
      optional UPnP action set. Real minimal DLNA servers (MiniDLNA
      included) do it this way — SCPD as an honest capability list, not a
      spec checklist — so that's the precedent followed here instead of
      the more "complete-looking" version originally planned.
- [x] `core::http::router` (`src/core/http/router.rs`): `fn route(method,
      path) -> Route` as a plain, synchronous, unit-tested match —
      `Route::DeviceDescription`, `Route::Scpd(ServiceType)`,
      `Route::NotFound`. The async handler (`HttpServer::handle`) is a
      thin `match` on top of this pure decision.
- [x] Wired into `main.rs`: `HttpServer::bind` on the resolved interface IP
      (not `0.0.0.0`), spawned alongside the SSDP tasks, aborted on the
      same shutdown path.
- [x] `tests/integration.rs`: `reqwest` as a dev-dependency (zero features
      — no TLS needed for plain HTTP, keeps it as light as reqwest gets),
      binds the server on ephemeral `127.0.0.1:0`, `GET`s all three URLs,
      asserts `200`/`404`, `Content-Type`, and well-formed XML.

**Exit criterion:** `curl` against `/description.xml` and both SCPD URLs
returns well-formed XML matching the UPnP device/service schema —
confirmed both by the integration test and by hand (`curl`) against a real
running instance reachable on the LAN.

---

## Phase 4 — Index, scanner, and `FolderMirror`

**Goal:** the server has an in-memory picture of the media directory and can
serve it as a 1:1 tree.

- [x] `index.rs`: `HashMap<ObjectId, Node>`-based index. Two shapes on
      purpose: internal `Node`/`NodeKind` storage (a container's real
      child-ID list) vs. the `Entry`/`Container`/`Item` view handed to
      callers (a container's child *count*, not the list — Browse only
      needs a count at that level). `IndexBuilder` is the only way to
      construct one, so ID assignment and parent/child linking happen in
      exactly one place.
- [x] `scanner.rs`: `walkdir`-based walk (chosen specifically for its
      symlink-loop handling), `exclude_patterns` support via a small
      hand-rolled `*`-only glob matcher (config-provenance, not
      network-provenance, so it's not on the fuzz list — just needs unit
      tests, which it has). Audio files recognized by extension
      allowlist, since there's no tag/metadata parsing (see
      `THREAT_MODEL.md`). One correction made while implementing:
      `exclude_patterns` applies to descendants of a configured
      directory, not the directory itself — otherwise a pattern like
      `.*` could silently exclude an entire configured library if its
      own folder name happened to start with a dot, with no error to
      explain why the library came up empty.
- [x] `content::folder::FolderMirror` implementing `trait ContentSource`
      (trait defined in `content/mod.rs`, per the spec's module
      convention). A thin wrapper for now — Phase 8's `MusicLibraryView`
      is where a `ContentSource` actually does real query logic.
- [x] Object-ID namespace: `"0"` is reserved for the root, per the UPnP
      ContentDirectory spec. Everything else is a plain per-scan integer
      — **no source-prefix scheme yet**, and that's deliberate: with only
      one `ContentSource` mounted, there's nothing to namespace against.
      The prefixing itself (MiniDLNA's `1$FF0` pattern) is
      `CompositeContentSource`'s job once Phase 8 actually mounts
      multiple sources at once — it intercepts `"0"`, presents the
      union of enabled views, and prepends/strips a `"{prefix}$"` before
      talking to each child source. Individual sources like `FolderMirror`
      never need to know a prefix exists.
- [x] Property test (`proptest`, new dev-dependency): generates 1-20
      arbitrary nested paths (shared prefixes become shared directories,
      so this produces real varied tree shapes) with a mix of audio and
      non-audio extensions, scans them, and walks the resulting index
      from root asserting every reachable child both exists and has its
      `parent_id` pointing straight back. 256 cases in ~0.2s.

**Exit criterion:** the property test passes, and the index matches a
fixture directory tree exactly after a scan — `exact_fixture_tree_matches_expected_shape`
in `scanner.rs` builds `Artist/Album/{01 - Track.flac, 02 - Track.mp3,
cover.jpg, .DS_Store}` and asserts the index contains exactly the two
audio files, correctly nested, with the image and dotfile excluded.

---

## Phase 5 — ContentDirectory SOAP dispatch and DIDL-Lite

**Goal:** a real client can Browse the tree and get correct XML back.

- [x] **Fixed a real architecture bug before writing any dispatch code.**
      The private planning spec contradicts itself: its prose says trait
      definitions belong in `core` so `core` never depends on
      `content`/`transform`/`metadata` ("implementations depend on core,
      never the reverse... that's what actually buys the flexibility"),
      but its file-tree sketch annotates `content/mod.rs` as where
      `ContentSource` lives — which is what Phase 4 followed. Moved the
      trait to `core::content_source` before `core::dispatch` could make
      the wrong-direction dependency permanent; `content::folder` now
      depends on `core`, not the other way around.
- [x] SOAP envelope parsing (`core::soap`, `quick-xml`), bounded to 8KB,
      checked against `Content-Length` before the body is even read
      (`core::http`) and enforced again in the parser itself. Deliberately
      syntactic rather than fully namespace-aware — `local_name()` strips
      whatever prefix a client's SOAP toolkit chose (`s:`, `SOAP-ENV:`,
      no prefix at all all resolve the same way), and *which* service is
      being invoked comes from the HTTP route, not from re-deriving it out
      of the body's declared namespace.
- [x] `core::dispatch`: routes ContentDirectory's `Browse`,
      `GetSearchCapabilities`, `GetSortCapabilities`, `GetSystemUpdateID`,
      plus ConnectionManager's `GetProtocolInfo`,
      `GetCurrentConnectionIDs`, `GetCurrentConnectionInfo` (moved from
      Phase 3); a proper SOAP fault (`Invalid Action`, code 401) for
      everything else on both services. `Browse` also honors
      `StartingIndex`/`RequestedCount` pagination (a real, well-scoped
      addition beyond the original task list — large libraries need it,
      unlike `Search`) and returns `NoSuchObject` (701) for an unknown
      `ObjectID`, `InvalidArgs` (402) for a missing/malformed argument.
- [x] `core::didl`: renders `index::Entry` directly as DIDL-Lite XML —
      no separate `MediaContainer`/`MediaItem` struct hierarchy, since
      `Entry` already carries everything a `<container>`/`<item>` element
      needs and a second shape would just be mapping boilerplate.
      `core::didl::format` is the one canonical audio-format table,
      shared with `scanner` (which now calls it instead of keeping its
      own extension list) so "what counts as audio" and "what's this
      format's protocolInfo" can't drift apart.
- [x] **Correction to this task as originally planned**: golden-file tests
      per FLAC/MP3/MP4/AAC/MKV/JPEG made no sense for an audio-only server
      that never scans video or images — that list was inherited from a
      generic DLNA-server template. Wrote inline-assertion tests (a
      literal expected string is a "golden file" too, just not a separate
      fixture, and these are short enough that a separate file would be
      pure ceremony) for the formats `scanner` actually recognizes: MP3
      (the one format that gets `DLNA.ORG_PN` — see `core::didl::format`'s
      doc comment for why the others deliberately don't), FLAC, M4A, OGG,
      WAV.
- [x] `fuzz/fuzz_targets/soap_parse.rs` — run for real (3.4M execs / 30s
      local run, no crashes), not just scaffolded, via the `fuzz_support`
      seam from `docs/DESIGN.md`'s encapsulation guidance.

**Exit criterion:** `BrowseDirectChildren` and `BrowseMetadata` against
`FolderMirror` on a fixture tree return correct DIDL-Lite — verified by
`tests/integration.rs` driving the real HTTP server end-to-end (a two-hop
Browse: root → album → item metadata) and by hand against a real running
instance on the LAN (`curl` with a real SOAP body; `Browse`,
`GetSystemUpdateID`, and an unimplemented `Search` action all behaved
exactly as designed, including the 500-with-SOAP-fault response).

---

## Phase 6 — HTTP file serving and Range

**Goal:** a client can actually play a file, including seeking.

- [x] **Reframed "path resolution" for this architecture, deliberately.**
      The spec's literal framing (`&str -> Result<PathBuf, Error>`,
      reject `..`/encoded traversal) assumes a URL that embeds a real
      path fragment. Ours doesn't: `/item/{id}` (decided in Phase 5) is
      an opaque `ObjectId` that only ever drives an index lookup — no
      attacker string is ever concatenated into a filesystem path, so the
      classic traversal bug class this guards against doesn't have a
      way in. The real residual risk is different: `follow_symlinks`
      letting a symlink *inside* the configured root resolve to a target
      *outside* it. That's what actually got built:
      `core::http::resolve_within_roots` canonicalizes an item's path at
      **serve time** (not just scan time — a symlink's target can change
      between scans) and verifies it's still under one of the
      canonicalized `media_roots`. Property-tested (see below), including
      a regression guard on the classic naive-string-prefix trap
      (`/media/music-private` must not look like it's inside
      `/media/music` — `Path::starts_with` compares components, not raw
      strings, so this passes, but the test pins that assumption).
- [x] `core::http::router::parse_item_path` — the one place raw,
      attacker-controlled request-path text actually gets parsed
      (extracting `{id}` from `/item/{id}`), so *this* is what
      `fuzz/fuzz_targets/path_resolve.rs` fuzzes, even though it's a
      narrower thing than spec's original framing (see above).
- [x] `core::http::range`: hand-rolled (not `http-range-header` — the
      parser ended up small enough that a dependency wasn't worth it),
      single range only, checked/saturating arithmetic throughout,
      handles all three RFC 7233 forms (`start-end`, `start-`, `-suffix`),
      416 for malformed, multi-range, or any range against an
      empty file.
- [x] `trait ByteSource` (`core::byte_source`) + `PassthroughSource`
      (`transform::passthrough`). Hand-rolled `Pin<Box<dyn Future>>`
      instead of the `async-trait` crate — boxing by hand at one call
      site is a few extra lines, not worth a dependency whose only job
      is that syntax. Reads are buffered into memory (`Bytes`), not
      streamed — deliberate MVP simplification: this project's target
      files are music tracks (a few MB), not video, and low resource
      footprint is explicitly "a natural consequence of narrow scope,
      not a thing to specifically optimize for" (spec goal 3). True
      streaming is one function to swap later if it ever matters.
      `supports_range()` is checked before honoring any `Range` header —
      false would mean serving the whole body with `200 OK` regardless,
      per the spec's design note (not exercised yet: `PassthroughSource`
      always returns `true`).
- [x] Response headers: `Content-Type`, `Content-Length`,
      `Accept-Ranges: bytes`, `contentFeatures.dlna.org` (reuses
      `core::didl::format`, refactored this phase into
      `mime_for`/`dlna_content_features` primitives that `protocol_info`
      itself now builds on, so Browse responses and item-serving headers
      can't drift apart), `transferMode.dlna.org: Streaming` (always —
      this server only ever serves audio). `HEAD` computes the identical
      response and strips the body, so headers can never drift between
      `GET` and `HEAD` for the same request.
- [x] `fuzz/fuzz_targets/range_parse.rs` (fuzzes both the header string
      and the file size it's checked against — 21.5M execs / 20s, no
      crashes) and `fuzz/fuzz_targets/path_resolve.rs` (27M execs / 20s,
      no crashes). **Also fixed while here**: the CI fuzz-smoke job was
      still only running `ssdp_parse` — `soap_parse` from Phase 5 had
      never actually been added despite the comment left saying it
      would be. Converted the job to a matrix over all four targets so
      this can't be missed again.
- [x] Property tests: `resolve_within_roots` never escapes the root for
      arbitrary real nested files (`proptest` + real tempdir fixtures,
      matching the style from Phase 4's scanner property test); `range`
      parsing is exhaustively unit-tested per RFC 7233 form and fuzzed
      for the "never panics" property, plus explicit tests that a 416
      response always carries a well-formed `Content-Range: bytes
      */{size}`.

**Exit criterion:** GET with and without `Range` headers streams a fixture
file correctly, including a mid-file seek — verified three ways:
`tests/integration.rs` (13 new assertions: full GET, `HEAD`, mid-file
range, suffix range, out-of-bounds → 416, multi-range → 416, unknown
item → 404, all against a real file on disk); by hand against a real
~4.8MB MP3 on a running instance on the LAN; and via
`examples/verify_item_playback.rs` — a small hand-rolled HTTP/1.1 client
over `TcpStream` (no `curl`, no new dependency, since our own responses
are always a plain `Content-Length` body, never chunked) that Browses a
running server to find a real item and exercises the same checks,
reproducibly. Run it with `cargo run --example verify_item_playback --
[host] [port]`.

---

## Phase 7 — Rescan timer

**Goal:** new files show up without a restart.

- [x] `src/rescan.rs`: `tokio::time::sleep`-based loop (not literally
      `tokio::time::interval` — a plain sleep-then-scan loop is simpler
      for "do a bounded-time operation, then wait" and doesn't accumulate
      missed ticks the way `interval` does if a scan ever runs long).
      Each scan runs on `tokio::task::spawn_blocking`, since directory
      walking is real filesystem I/O that shouldn't stall the async
      runtime. Rather than "diff against the index, update in place," a
      full rescan rebuilds the whole `Index` from scratch and swaps it in
      wholesale — matches "MVP: in-memory index, rebuilt on scan" from
      the private planning notes, and means no incremental-diff logic to
      get subtly wrong. Object IDs are reassigned each rescan as a
      result; nothing depends on an ID staying stable across rescans
      (every Browse is a fresh round-trip), so this is an accepted
      tradeoff, not an oversight.
- [x] `rescan.on_startup` config option — genuinely wired now, not just
      parsed. **Real design decision made here**: the server binds
      HTTP/SSDP with an *empty* index immediately, then the rescan task
      (spawned right after) does the first populate — rather than
      blocking startup on a scan before the server starts listening.
      SSDP/`description.xml` become reachable immediately; the library
      fills in moments later. `on_startup=false` means exactly what it
      says: the library stays empty until the first scheduled interval,
      which could be hours away on the default 4h cadence — that's the
      config option's actual purpose (skip a potentially-slow scan
      blocking startup), not a bug.
- [x] `index::SharedIndex` (new): a cheap-to-clone (`Arc<RwLock<Index>>`
      underneath) handle that `rescan::run` replaces wholesale and any
      `ContentSource` reading through a clone of the same handle sees
      immediately. Added deliberately at this layer rather than inside
      `FolderMirror` alone: the spec's own words for Phase 8's
      `MusicLibraryView` are "queries the *same* underlying Index
      `FolderMirror` reads from" — meaning the index has to be a shared
      resource multiple `ContentSource`s hold a handle to, not
      `FolderMirror`'s private state. Getting this right now avoids
      Phase 8 needing to redesign it. (This is the "Rescan invalidates/
      recomputes the live-computed views from Phase 8" bullet from the
      original task list — there's nothing extra to invalidate once the
      views themselves read live off the same `SharedIndex`, which is
      exactly the point of building it this way.)

**Exit criterion:** a file added to the media directory appears in Browse
results after one rescan interval, with no restart — verified three ways:
`rescan::run`'s own unit tests (on_startup timing, a real scan replacing a
`SharedIndex`); `tests/integration.rs`'s
`new_file_appears_after_one_rescan_interval_with_no_restart` (real HTTP
server, 50ms interval, a file written to disk mid-test, Browse before and
after); and by hand against a real running instance — 6 real MP3s, a 3s
interval, added a 7th file, waited 5s, `NumberReturned` went from 6 to 7
with the new file present, server never restarted.

---

## Phase 8 — Music library views

**Goal:** Albums, Artists, and Recently Added — the actual reason this
project exists instead of just running MiniDLNA.

- [x] `core::metadata_provider::MetadataProvider` + `metadata::filename::FilenameMetadata`.
      **Narrowed from the spec's literal wording.** SPEC.md §10 lists
      title, artist, album, track, and genre as this trait's job. But
      §5.1, the section that actually defines the MVP feature, groups
      albums and artists by folder structure, not by a parsed tag. Two
      grouping mechanisms would disagree on real, messy libraries, so
      this trait now does only what §5.1 leaves for it: pull a leading
      track number and a clean title out of a filename. A later
      tag-reading provider can add real artist/album/genre fields; the
      `Metadata` struct already leaves room for them.
- [x] `content::music_library::MusicLibraryView`: All Songs, Albums,
      Artists, Recently Added Songs, Recently Added Albums. Albums and
      Artists reuse the real folder tree `FolderMirror` already reads
      from `SharedIndex` — an album is a track's direct parent
      container, an artist is that container's own parent. No new data
      structure, no separate grouping pass.
- [x] `content::composite::CompositeContentSource` mounts the views
      listed in `library.views`, each under its own ID prefix
      (`"albums$42"`), joined with `$` — chosen over MiniDLNA's `/`
      because the router already rejects `/` inside an object ID
      (`core::http::router::parse_item_path`), so `$` needed no router
      change. An unknown prefix, or a known prefix with an ID its mount
      doesn't recognize, returns `None` — UPnP fault 701, same
      fail-closed contract every `ContentSource` in this project keeps.
- [x] Recently Added Songs and Recently Added Albums compute their list
      fresh from the index on every Browse call. There's no cache to
      invalidate, so a rescan can never leave a stale result behind.
- [x] Config: `library.views`, `library.recently_added` (`songs_count`,
      `albums_count`, `max_age_days`) — both already existed as parsed,
      unused fields since Phase 1; this phase is what reads them.
- [x] Property test: an arbitrary real directory tree, scanned for real,
      browsed through Albums and Artists, checked for two things at
      every step — no panic, and every returned entry's `parent_id`
      matches the container it came from. This test caught three real
      bugs, described below.

**Bugs the property test found, and the fix for each:**

1. **Wrong `parent_id` at the top level.** An album three folders deep
   is still a *direct* child of the "Albums" view — but the code first
   drafted just handed back the real index entry, real folder parent and
   all. Fix: every top-level entry gets its `parent_id` overwritten to
   this view's own root (`reparent_to_root`), and `entry()` matches that
   override for the same ID.
2. **A folder holding both a track and a sub-folder.** Say `Music/`
   holds `track.mp3` directly, and also a `Bonus/` sub-folder with its
   own track. `Music` is genuinely an album (it holds a track). But
   naively drilling into it returned the real folder's children — the
   track *and* the `Bonus` container — even though `Bonus` is a
   separate album in its own right, not more of `Music`'s content. Fix:
   drilling into an album now filters to tracks only
   (`tracks_of_album`); drilling into an artist filters to sub-folders
   that are themselves albums (`albums_under_artist`).
3. **A folder that is both an album and an artist.** Nest deep enough
   (`Artist/SelfTitled/track.mp3` next to
   `Artist/SelfTitled/Bonus/track2.mp3`) and `SelfTitled` qualifies as
   an album (it holds a track) *and* as an artist (`Bonus` is a
   sub-album of it). Showing it in both roles at once gave `entry()` two
   different correct answers for the same ID. Fix: a folder that
   qualifies as both keeps only its artist role; its own direct track
   drops out of this view. Same kind of trade-off as the orphan-track
   exclusion below — documented in `albums_under_artist`'s doc comment,
   not silently swallowed.

Bug 1 also hid a `childCount` mismatch: an album's displayed count must
match what browsing it actually returns (tracks only), not the real
folder's raw child count. Fixed alongside bug 2.

**Design decisions, from the planning conversation before this phase's
code was written:**

- Recently Added's age basis is a file's `Item.modified` time from the
  index, not directory mtime — already scanned, so no extra filesystem
  call.
- A filename's leading `"NN - "` becomes a track number for in-album
  sort order (`FilenameMetadata::parse_track_number`).
- A track with no real containing-album folder — one sitting directly in
  a configured media directory, whose only real parent is the
  media-mount container itself — is left out of Artists (the album's
  own parent there is the root, and an album with no artist above it has
  no Artists entry). It still appears under Folders and Albums, so
  nothing is hidden, only ungrouped.
- No new `cargo-fuzz` target: `CompositeContentSource`'s prefix split
  (`split_once('$')`) is a pure, total string operation on IDs that
  already pass through the fuzzed `parse_item_path` → `ObjectId::new`
  path. There's nothing new here for a fuzzer to reach.

**Verified:** `cargo test --all-features` (149 lib tests, 16 integration
tests, including the property test above); `cargo clippy --all-targets
--all-features -- -D warnings` and `cargo fmt --check` both clean; and
by hand against a real running instance — two artists, three albums (one
with a mixed track-plus-loose-file folder), and a root-level track with
no artist folder above it. Root browse showed exactly the five
configured views. `Albums` showed the mixed folder with `childCount="1"`
matching its one real track, not its raw folder count of two.
`RecentlyAddedSongs`, configured with `songs_count = 5` against ten real
tracks, returned exactly five. `Artists` correctly excluded the
root-level track's album. Playing a track through its composite-prefixed
URL served the exact bytes of the real file on disk. An unknown mount
prefix and an unknown ID inside a known prefix both returned UPnP fault
701.

That first manual check was ad hoc `curl`. Following Phase 6's own rule
— a manual check belongs in a committed, reproducible script, not a
one-off terminal session — it's now `examples/verify_music_library.rs`:
same hand-rolled-`TcpStream` style as `examples/verify_item_playback.rs`,
no `curl` dependency. It walks whatever tree the running server actually
returns (it doesn't assume a fixed `library.views` list), checking at
every container that `childCount` matches what browsing in actually
returns and that every entry's `parentID` names the container just
browsed — the exact two bugs above, turned into a standing regression
check against a real server, not just the in-process property test.
Writing it caught a real bug immediately, in the script itself: an
`<item>` tag appearing before a `<container>` tag in a response made its
first regex-free tag scan skip the item entirely. Fixed by comparing tag
positions directly instead of preferring one tag name over the other.

**Exit criterion:** Browsing the root shows exactly the configured views;
adding more than 50 items doesn't break Recently Added (the MiniDLNA bug
this feature exists to avoid) — met, verified above with a
`songs_count = 5` / ten-track case.

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
