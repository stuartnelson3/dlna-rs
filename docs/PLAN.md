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

**A fourth bug, found after this phase had already shipped**, against a
real media server on the user's LAN rather than a test fixture:

4. **A quadratic Browse.** `tracks_of_album` and `albums_under_artist`
   each called `album_ids()`/`artist_ids()` to check whether an ID was
   real — and each of those re-walked the *entire* index from scratch.
   `albums()` and `artists()` call those two functions once per album or
   artist they find. On the tiny fixtures every test and manual check
   here used (a dozen or so tracks), a full re-walk per album is free.
   On a real library the user pointed a running instance at, one Browse
   of the root took long enough that the client gave up and disconnected
   before any response came back — `HTTP connection error: connection
   closed before message completed` in the server's own log was the
   first sign, then a synthetic 10,000-track, 1,000-album, 200-artist
   fixture reproduced it directly. Fix: `MusicLibraryView::snapshot()`
   now walks the index once per Browse call and returns every item plus
   the derived album/artist ID sets; every other method takes that
   `&Snapshot` instead of re-deriving it. Verified against that same
   10,000-track fixture afterward: root Browse in 34ms, an artist's five
   albums in 4.5ms, `childCount` still correct on every one of them. The
   property test in this phase's own checklist never caught this, because
   `proptest`'s generated trees stay small by construction — a reminder
   that a property test proves an invariant holds, not that it holds
   *fast enough*.

   That gap is closed now: `browsing_a_large_library_stays_fast` builds
   the same 10,000-track, 1,000-album, 200-artist shape directly through
   `IndexBuilder` (no real files, no scanner — this test is about
   `MusicLibraryView`'s own complexity, not disk I/O) and asserts every
   Browse call finishes in under 1.5 seconds. Measured headroom is large
   — the whole test runs in about 60ms in an unoptimized debug build,
   two orders of magnitude under the threshold — deliberately, so it
   stays green on a slower or loaded machine but still fails hard the
   moment an O(albums) or O(tracks) re-walk per item sneaks back in. A
   real, deliberate slowdown should fail this test and force whoever
   made the change to raise the threshold on purpose, with a reason
   written next to it — not slip past unnoticed.

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

- [x] All four fuzz targets run crash-free for a sustained local run (not
      just the CI smoke duration). `cargo fuzz run <target> --
      -max_total_time=120`, all four, no crash, no hang:

      | target | executions | duration |
      |---|---|---|
      | `ssdp_parse` | 118,031,297 | 121s |
      | `soap_parse` | 5,443,177 | 121s |
      | `range_parse` | 121,125,634 | 121s |
      | `path_resolve` | 122,048,276 | 121s |

      `soap_parse`'s lower rate matches Phase 5's own note that XML
      parsing costs more per input than the other three's plain string
      parsing. That is not a red flag. It is the same shape as every
      prior fuzz session this project has run.
- [x] `cargo geiger` baseline report reviewed. `dlna-rs 0.1.0` itself
      reports `0/0` on every column (functions, expressions, impls,
      traits, methods). This confirms, with an independent tool, what
      `#![forbid(unsafe_code)]` already guarantees at compile time for
      our own code. The dependency tree carries real unsafe code, mostly
      in `tokio`, `bytes`, `memchr`, `http`, and `socket2`. That is
      expected: I/O and byte-level parsing are exactly where unsafe buys
      real performance. This matches docs/THREAT_MODEL.md's existing
      stance: geiger is a periodic look at the dependency tree, not a
      CI gate, since a raw count of unsafe blocks says nothing about
      whether any of them are wrong.
- [x] systemd unit with the hardening directives from the threat model
      (`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`,
      `PrivateTmp`, etc.). **`systemd/dlna-rs.service`.** Every
      sandboxing directive in it (all but `User`/`Group`/`ReadOnlyPaths`,
      which need a real deployment to check) was tested against a real
      running instance, not just written from documentation. A
      transient unit under `systemd-run --user` used the full directive
      set: `NoNewPrivileges`, `ProtectSystem=strict`,
      `ProtectHome=read-only`, `PrivateTmp`, `ProtectKernelTunables`,
      `ProtectKernelModules`, `ProtectKernelLogs`, `ProtectControlGroups`,
      `ProtectClock`, `ProtectHostname`, `RestrictSUIDSGID`,
      `RestrictRealtime`, `LockPersonality`, `MemoryDenyWriteExecute`,
      `RemoveIPC`, and `RestrictAddressFamilies=AF_INET AF_NETLINK`. It
      started cleanly, resolved its network interface, joined SSDP
      multicast, served a real `BrowseDirectChildren` request, and
      served `description.xml`. A second run with
      `RestrictAddressFamilies=AF_INET` alone (no `AF_NETLINK`) is the
      negative control: it failed loudly at startup with `no IPv4
      address found for interface "lo"`. This proves `if-addrs`'
      interface resolution genuinely needs `AF_NETLINK`, and that
      dropping it fails safe: a clear error, not a silent misbehavior.
      Worth confirming directly; documentation alone would only be a
      guess.
- [x] Resolve `DynamicUser=true` vs. a static service user, specifically
      against an NFS-mounted media directory. **A static user.**
      `DynamicUser=true` picks a fresh, random UID on every start. An NFS
      export that checks the caller's UID or GID can then refuse access
      on a later restart even though an earlier one worked, since the
      export was never told about this run's UID. Real, common setups
      do exactly this: `no_all_squash`, NFSv4 ID mapping, a
      Kerberos-secured export. A static, named user's UID never changes,
      so the operator grants it read access once, using `chown`, an
      ACL entry, or an `exports(5)` line, and it keeps working across
      restarts. The shipped unit documents `DynamicUser=true` as a fine
      alternative for a purely local-disk setup, where this risk does
      not apply. The tradeoff is named, not silently picked.
- [x] Confirm the release profile has `panic = "unwind"`, and that a panic
      in one connection task doesn't take down the process. `Cargo.toml`
      already had `panic = "unwind"` with this exact reasoning in a
      comment (from Phase 0). The "doesn't take down the process" half
      is now a real test, not just a read of `serve()`'s doc comment:
      `core::http::tests::a_panic_in_one_connection_task_does_not_take_down_the_server`
      binds a real `HttpServer` with a `ContentSource` that panics on
      one specific ID, sends a request that hits it, and confirms the
      connection drops. That is the correct outcome: a panicking
      handler must never look like a normal response. A second request,
      on a fresh connection, still gets a normal `200`.

**Exit criterion:** fuzz targets run 60–120 seconds crash-free locally; the
systemd unit is reviewed against the threat model line by line. Met. See
the evidence above.

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

- [x] Test against at least one real hardware renderer and one software
      client (VLC, BubbleUPnP). **Verified by the user directly**, against
      a real media server on their LAN (see Phase 8's bug 4 for how that
      server got found in the first place): the DTS Play-Fi app's
      built-in media server browser found `dlna-rs` over SSDP, listed its
      library, and played back both FLAC and MP3 files.
- [x] Verify seeking works during playback, not just that the file opens.
      **Verified by the user**, same Play-Fi session: seeking worked, and
      the user's own words were "was fast, and the recently added albums
      stuff worked correctly." A 24-bit FLAC specifically was confirmed
      streaming and seeking correctly too.
- [x] Finalize `docs/DESIGN.md` and `docs/THREAT_MODEL.md`. `DESIGN.md`'s
      "Current state" now covers Phase 9 (a verification pass, not a
      design change: nothing in `src/` gained a module or a trait) and
      Phase 11's real-client sign-off. `THREAT_MODEL.md`'s fuzz numbers
      were pointing at the original short Phase 2/5/6 runs (20 to 30
      seconds each); updated to the longer, more rigorous Phase 9 runs
      (120 seconds each, higher executions), and the panic-isolation
      claim now cites the real test that proves it instead of only a
      doc comment.
- [x] Fill in the README's Building and License sections for real. Both
      already held real content from earlier session work (the actual
      dual-license text, real build instructions, the musl static-build
      option). What was genuinely stale was the Status section, still
      claiming "Phases 0 through 8" after Phase 9 and half of Phase 11
      had already landed. Fixed, and Building now points at
      `systemd/dlna-rs.service` for anyone wanting to run this as a
      real service.

**Exit criterion:** discoverable and playable, seek included, on real
hardware and one software client; clean `cargo audit`/`cargo deny`; zero
`unsafe` in first-party code; fuzz targets crash-free for the scheduled
duration. This is MVP v0.1.

**Met.** Real-client discovery, browse, playback, and seek: verified by
the user against the DTS Play-Fi app (above). `cargo audit`: 0
vulnerabilities across 161 dependencies. `cargo deny check`: advisories,
bans, licenses, and sources all pass (three warnings about license
allowances in `deny.toml` that no current dependency happens to use,
not a failure). Zero `unsafe` in `dlna-rs` itself, and all four fuzz
targets crash-free for a sustained 120-second run: both confirmed in
Phase 9. **This is MVP v0.1.**

---

## Phase 12 — Real tag metadata and embedded cover art

**Goal:** real `artist`/`album`/`genre` tags and embedded cover art, on
top of the filename/folder heuristics MVP shipped with.

A post-MVP research pass compared `symphonia` (the library named in the
original private planning notes) against `lofty` for this job. Verdict:
`lofty`. `symphonia` has no metadata-only mode. Enabling its MP3/MP4
demuxers bundles real decoder crates this project would never call,
since it never transcodes. `lofty` is purpose-built for tag and
embedded-picture reading: about a dozen small dependencies, mostly
compression/checksum utilities for tag blocks, and no audio codec among
them (confirmed with `cargo tree -p lofty` after adding it).

Adding `lofty` surfaced one real, if minor, supply-chain finding:
`cargo audit`/`cargo deny` both flag `paste` (a proc macro `lofty_attr`
depends on) as RUSTSEC-2024-0436, "unmaintained." Not a vulnerability:
the author archived the repository as feature-complete, and `paste`
runs only at compile time, shipping no code into the built binary.
Accepted and recorded, not silently ignored: `deny.toml` now has an
explicit `ignore` entry for this one advisory ID, with the reasoning
written next to it.

- [x] Extend `core::metadata_provider::Metadata` with `artist`, `album`,
      `genre`, and `has_art`: fields its own doc comment already
      promised in Phase 8. `metadata::filename::FilenameMetadata`
      (unchanged, still the cheap, no-file-I/O implementation used every
      time `content::music_library` sorts a container's children) fills
      them with `None`/`false`. A new `metadata::tags::TagMetadata`
      fills all four with a real `lofty` read, used only by the
      scanner, once per file, with the result cached on `index::Item`
      (new `TrackTags` struct). It is never re-read per Browse call.
      Reading a tag on every Browse instead of once at scan time would
      repeat Phase 8's bug 4: real work, done once, turned into real
      work done on every request.
- [x] `lofty` is named in exactly one file, `src/metadata/tags.rs`. Its
      own `MimeType` enum never crosses the `ArtSource` trait boundary.
      A small internal match maps it to this crate's own `&'static str`
      MIME constants, the same fallback convention
      `core::didl::format::mime_for` already uses. A future swap to a
      different tag library touches only that one file: the
      encapsulation rule doing exactly the job it exists for.
- [x] New `core::art_source::ArtSource` trait, separate from
      `ByteSource`: serving a whole audio file and pulling a picture out
      of a tag block are different jobs, and conflating them would force
      `transform::passthrough::PassthroughSource` to grow a capability
      it has no business having. It reads fresh from disk on every call.
      No picture bytes are cached in the index, the same "don't hold
      large blobs in memory" discipline `ByteSource` already follows, so
      a library with thousands of embedded covers doesn't bloat memory
      use.
- [x] New route, `GET /art/{id}`, mirroring `/item/{id}`:
      `core::http::router::parse_art_path` is a literal copy of
      `parse_item_path` with a different prefix (two small, single-
      purpose functions, not one parameterized over a prefix, matching
      this module's existing `item_route`/`scpd_route` shape), and
      `handle_art` reuses `verify_within_roots` as-is. No `HEAD` and no
      `Range` support for art. Real embedded covers are small, a single
      whole-body response is enough, and that's a deliberate, named
      scope call, not an oversight. `parse_art_path` was added to the
      `path_resolve` fuzz target alongside `parse_item_path`, since it's
      now the same kind of attacker-reachable parsing site.
- [x] DIDL-Lite renders `<dc:creator>` and `<upnp:artist>` together when
      an artist is known (real DLNA clients disagree on which one they
      read, so emitting both costs nothing), `<upnp:album>`/
      `<upnp:genre>` when present, and `<upnp:albumArtURI>` when
      `has_art` is true. Each element is left out entirely, never
      rendered empty, when its source field is absent. No
      `dlna:profileID` attribute on the art URI: this server serves the
      embedded picture byte-for-byte with no resizing, so a profile
      claim like `JPEG_TN` (a specific pixel size) would be a promise it
      can't back. Omitting it also means no new XML namespace was
      needed. Every new field is user-controlled binary tag data and
      goes through the same `escape()` helper `title` already used.

**Verified:** `cargo build`/`test`/`fmt --check`/`clippy -D warnings`
all clean (166 lib tests, 19 integration tests). `cargo audit` and
`cargo deny check` both pass clean with `lofty` added (the one real
finding, `paste`'s unmaintained advisory, is accepted and recorded in
`deny.toml`, not silently passed over). The `path_resolve` fuzz target
ran 25.9M executions in 20s with the new `parse_art_path` included, no
crashes. And by hand against a real running instance, with
a real tagged MP3 carrying a real embedded 1x1 PNG: `Browse` on the
track showed every new element with the real values, and no
`dlna:profileID`. `GET /art/{id}` returned `200`, `Content-Type:
image/png`, and the exact embedded PNG bytes, confirmed byte-for-byte.
An unknown art ID and an item with no embedded picture both returned
`404`. A `HEAD` request to `/art/{id}` also returned `404`, confirming
the deliberate scope decision holds.

**Exit criterion:** a real tagged file's artist/album/genre appear in
Browse results, and its embedded cover art is fetchable over HTTP. Met,
verified above.

---

## Phase 13 — External cover-art files

**Goal:** find an album's cover art the way most real collections
actually store it — a loose image file beside the tracks — not only an
embedded picture inside one file's own tag.

Real-world use turned up two problems with Phase 12's cover art: it only
read a picture embedded in a track's own tag, and most real albums don't
embed one. A survey script run against a real ~980-album library (see
`scripts/survey_cover_art.sh`) confirmed this directly: about half the
library used a loose `cover.jpg`/`folder.png`-style file or an
`Artwork`/`Scans`-style subfolder, plus a further chunk explained by
multi-disc albums (`Album/CD1/track.flac`) that keep their art one
level up, beside the disc subfolders rather than inside them. The first
version of the survey script under-counted this badly at first — it
checked cover-file names case-sensitively while checking audio
extensions case-insensitively, and it never looked one directory up at
all — corrected once real output looked suspiciously high on "none".

- [x] New `metadata::cover_files` module: a pure filesystem lookup, no
      tag parsing, no dependency on `lofty`. `find_cover_file` checks a
      named file (`cover`/`folder`/`front`/`albumart`/`album`/`art`/
      `thumb`, any of `.jpg`/`.jpeg`/`.png`, case-insensitive), then any
      other image file present (sorted, for a deterministic pick — real
      collections often use an arbitrary name like
      `AlbumArt_Large.jpg`), then an `Artwork`/`Scans`-style subfolder
      (matched by substring, since real folder names vary too much to
      enumerate). Both the track's own directory and the directory
      above it get the same three checks, in that order, since the
      track's own directory always wins when it has an answer.
- [x] `metadata::tags::TagMetadata` tries an embedded picture first (an
      embedded picture was deliberately attached to that exact track,
      so it takes priority), then falls back to `find_cover_file` for
      both `has_art` and `ArtSource::art`. `lofty` still never leaves
      `tags.rs`; the new module knows nothing about tags or `lofty` at
      all — a genuinely separate concern composed at one call site, not
      merged into it.
- [x] Root containment: checking the directory *above* the track's own
      could otherwise step outside a configured media directory — the
      one layout where that happens is a track sitting directly in the
      configured root, with no album folder wrapping it, so the
      directory above it is the root's own parent. `TagMetadata` now
      carries the configured media roots (mirroring
      `core::http::HttpServer`'s own `media_roots`, canonicalized in
      `TagMetadata::new` the same way) and refuses to climb into a
      directory outside them — the same containment property
      `core::http::resolve_within_roots` enforces for every file this
      server serves, applied here too rather than assumed.

**Verified:** `cargo build`/`test`/`fmt --check`/`clippy -D warnings`
all clean (200 lib + integration tests). New unit tests in
`metadata::cover_files` cover every lookup rule, including a folder
whose own name happens to contain an art-hint substring (`Bartok`
contains "art") not matching itself, and a regression test proving the
lookup refuses to return a path outside the configured root even when a
plain "check the parent directory" search would have found one. A new
integration test serves a real cover file over `GET /art/{id}` for a
track with no embedded picture, end to end.

**Exit criterion:** a track with no embedded picture, sitting beside a
`cover.jpg`, `folder.png`, or `Artwork/` subfolder, shows
`<upnp:albumArtURI>` in Browse output and serves the real image bytes
over `GET /art/{id}`. Met, verified above.

---

## Phase 14 — Tag-aware Albums/Artists grouping

**Goal:** prefer a real tag over folder structure for an album's
displayed title and its artist grouping, when the tag is trustworthy —
including unifying the same artist's albums across different real
folder shapes in the same library.

Albums/Artists grouped by folder structure alone since Phase 8, chosen
before any tag data existed, specifically to avoid two grouping
mechanisms disagreeing on a messy library. Real use surfaced the real
gap this leaves: many real albums are flat single-level folders
directly under the media root (`AC-DC - Back in Black [2003 Epic
Records Remaster]/track.flac`), with no wrapping artist folder — such
an album's "artist" (its folder's parent) is the media root itself, so
it was excluded from the Artists view entirely. Worse, the same real
artist could appear split across a proper nested tree and several flat
top-level folders in the same library, each showing up unrelated and
ungrouped instead of as one artist.

- [x] Album identity stays exactly what it was: the real `Index`
      container that directly holds a track, same `ObjectId`. Only its
      *displayed title* and *artist grouping* change, and only when a
      real tag is trustworthy: `consistent_tag_value` checks every
      track directly in that folder that carries the tag, and returns
      the shared value only if none of them disagree (case/whitespace
      folded). A missing tag on some tracks is a non-vote, not a
      disagreement; a real disagreement (a genuine compilation) falls
      back to the exact folder-based behavior Phase 8 shipped — no
      crash, no fabricated single-artist bucket. A proper "Various
      Artists" grouping stays a separate, deferred backlog item (see
      After MVP below), not something this phase builds.
- [x] Artist identity is now one of two kinds, resolved once per album,
      per `Snapshot`: `Folder(ObjectId)` (Phase 8's exact heuristic,
      unchanged) or `Tag(String)` (a normalized artist name). A `Tag`
      artist has no backing real `Index` container — it can span
      several real folders, or none consistently — so its `Container`
      is hand-built with a synthetic id (`tag-artist:<normalized>`,
      never colliding with a real `Index`-allocated id, since those are
      always plain integer strings) and a deterministic display title
      (the lexicographically smallest raw variant across every album
      that resolved to it, not whichever a `HashMap` happens to iterate
      first).
- [x] `content::music_library::MusicLibraryView`'s internals were
      restructured around this: `snapshot()` now resolves every album's
      display title, artist key, and the final artist→albums map in one
      pass; a new `album_container()` is the single place every album
      `Container` gets built, so `albums()`, `albums_under_artist()`,
      `recently_added_albums()`, and `entry()` can never disagree about
      a title. `containers_for()`/`with_real_child_count()` were
      deleted — no longer needed once every `Container` is correct at
      the point it's built.
- [x] `albums_under_artist()` now explicitly overwrites each returned
      album's `parent_id` to the artist being browsed, rather than
      trusting whatever the real `Index` recorded — necessary the
      moment a tag redirects an album to an artist other than its real
      folder-parent (a nested album whose own tags point to a different,
      tag-unified artist than its real parent folder).
- [x] The existing "a folder can be both an album and an artist" dual-
      role exclusion (Phase 8) is now computed globally, over the final
      resolved artist→albums map, instead of per-listing — required
      once a tag can pull an album out from under its real folder-
      parent's own artist bucket into an unrelated tag-derived one.

**Verified:** `cargo build`/`test`/`fmt --check`/`clippy -D warnings`
all clean (207 lib + integration tests). Every pre-existing test in
`music_library.rs` passes unchanged — traced to be byte-identical for
any untagged library, since `consistent_tag_value` on an all-`None`
tag list always returns `None`. Six new unit tests cover: a nested
album and a flat top-level album sharing one consistent artist tag
merging into one artist; artist tags differing only in case/whitespace
merging with a deterministic display title; inconsistent artist tags
falling back to folder grouping with no crash and no fabricated
bucket; a consistent album tag overriding the folder name for display,
with `Mode::Albums`'s listing and `entry()`'s direct lookup agreeing;
the inconsistent-album-tag fallback case; and a
`walk_and_check_consistency` pass against a tag-driven fixture, the
strongest guard on the `parent_id`-reparenting correctness fix. One new
`composite.rs` test confirms a tag-derived id containing a literal `$`
still round-trips through the `prefix$local_id` split. And by hand
against a real running instance: a real mixed library (a nested
`AC_DC/TNT` album, a flat top-level `AC-DC - Back in Black` album, both
tagged `artist: AC/DC`, plus one untagged album) showed exactly two
Artists entries — a merged "AC/DC" with `childCount="2"` listing both
real albums with correct `parentID`s, and the untagged album's
folder-based "Untagged" artist, unchanged — while the flat Albums view
kept listing all three real album folders exactly as before.

**Exit criterion:** the same real artist's albums, scattered across
different real folder shapes, merge into one Artists entry when their
tags agree; an untagged or inconsistently-tagged library's behavior is
unchanged. Met, verified above.

---

## Phase 15 — Persistent tag-read cache (redb)

**Goal:** an unchanged file's tags come from a small on-disk cache
instead of a real `lofty` parse, on every rescan after the first —
including across a process restart — fixing the real complaint that
scanning a large library was taking a long time on every single
rescan, not just the first one.

A prior research pass compared `redb` against a full pure-Rust SQLite
rewrite (`turso`) for this. Verdict: `redb`, used narrowly as a
`(path, size, mtime) -> tags` cache, not a full persisted index with
stable object IDs — nothing in this project needs ID stability across
rescans (`rescan.rs`'s own doc comment already said as much, since
Phase 7).

Real `redb` 4.2.0 needs rustc 1.90; this project's `rust-version` was
1.85, so adding it meant a real, deliberate MSRV bump, not a silent
one — confirmed with the user before making it.

- [x] New `metadata::tag_cache::CachedTagMetadata<P>`: generic over the
      inner `MetadataProvider` (real production code always
      instantiates it over `TagMetadata`; the generic exists so this
      module's own tests can substitute a call-counting fake instead of
      exercising real `lofty` parsing). `redb` is named in exactly one
      file — the same encapsulation rule `metadata::tags` already
      follows for `lofty`. Only `artist`/`album`/`genre`/`has_art` are
      cached; title and track number stay uncached (cheap,
      filename-only, so a rename shows up immediately with no
      invalidation logic needed for them).
- [x] Every write uses `Durability::None`, not the default
      `Durability::Immediate` — this cache is fully rebuildable, so a
      crash losing the last few writes just means a few files get
      re-parsed next time, never a correctness problem, and the
      speedup is real: `Durability::None` is what makes a write cheap
      enough to do on every scanned file.
- [x] New `[tag_cache]` config table (`enabled`, `path`), off by
      default — the first feature since Phase 12 to get a config
      toggle, because enabling it means writing a file to disk, and
      `systemd/dlna-rs.service`'s `ProtectSystem=strict` grants no
      writable path anywhere by default. `Config::load`'s validation
      pass rejects `enabled = true` with an empty `path` at load time,
      not as a runtime surprise. A comment in the unit file documents
      the matching `StateDirectory=dlna-rs` needed to actually use it.
- [x] Deletes are handled, not just documented as a limitation: a new
      `MetadataProvider::retain_only` method (default no-op) lets
      `rescan_once` tell whatever provider it's using which paths the
      scan actually found, once per rescan. `CachedTagMetadata`
      overrides it to drop any cached entry for a path no longer
      present — a two-pass read-then-write, safe as a rebuildable
      cache even with a race between the passes. Without this, a
      long-running deployment's cache file would grow forever as files
      get removed from the library over time.
- [x] Real bug found and fixed along the way, unrelated to caching
      itself but found while touching `scanner.rs`: an item's DIDL
      title was always the raw filename (`"01 - Track.mp3"`), never
      the parsed one (`"Track"`) `FilenameMetadata`/`TagMetadata`
      already computed — the scanner discarded `read.title` and reused
      its own separately-derived, unparsed filename string instead.
      Fixed to use the parsed title; every test asserting the old raw
      filename as a title was updated to the correct parsed value.

**Verified:** `cargo build`/`test`/`fmt --check`/`clippy -D warnings`
all clean (219 lib + integration tests). `cargo audit`/`cargo deny
check` both clean with `redb`/`serde_json` added — no new advisories,
license, or ban findings, only the pre-existing accepted `paste`
finding. New unit tests in `tag_cache` cover hit/miss/prune behavior
against a counting fake provider (isolated from real `lofty`
behavior), a garbage cached value degrading to a miss rather than a
panic, and persistence across separately-opened instances at the same
path (simulating a restart). A new perf tripwire in `scanner.rs`
scans a real 300-file tagged fixture twice against a `CachedTagMetadata`
and asserts the warm pass is at least 2x faster than the cold one —
measured on this machine: **176.5ms cold, 3.8ms warm, about a 45x
speedup**. A new `rescan.rs` test confirms `retain_only` is called with
exactly the files still present after a file is deleted between two
rescans. By hand against a running instance: `[tag_cache]` enabled
against a real tagged library created the `.redb` file, Browse output
was unaffected, and the cache file persisted correctly across a
process restart.

**Exit criterion:** a rescan of an unchanged library is measurably
faster with `[tag_cache]` enabled than without it, and the cache
survives a restart. Met, verified above.

---

## Post-Phase-15 fixes, found by querying a real deployed instance

Two real bugs, found by browsing the user's actual real running server
directly over the network (not guessed, not from a synthetic fixture)
right after Phase 14/15 shipped.

**A real DLNA client showed the entire Artists view as empty**, even
though the server's own raw `ContentDirectory` response had all 447
real entries. Root cause: Phase 14's synthetic tag-derived artist IDs
(`tag-artist:<name>`) are built from the raw tag text, which can
contain `&`, unlike every other ID in this project (always a plain
digit string, never needing escaping before). `core::didl::render_one`
wrote `id`/`parentID` attributes straight from `ObjectId`'s `Display`
impl, with no escaping — so an artist like "Art Blakey & The Jazz
Messengers" produced a raw, unescaped `&` inside an XML attribute
value, which makes the *entire* DIDL-Lite document unparsable, not
just that one entry. Confirmed directly: fetched the real server's
full Artists listing, decoded the SOAP-escaped body, and fed it to a
real XML parser, which failed at exactly that entry. Fixed by escaping
`id`/`parentID` the same way `title` already was; new tests render a
container/item with a `&` in a synthetic ID and assert the whole
document parses.

**Some track titles kept a leading number**, e.g. "01 Just Friends"
instead of "Just Friends". Root cause: a disc-track filename
convention (`"1-01 Just Friends.mp3"`, disc 1 track 01) — `metadata::
filename::parse_track_number` only ever stripped one leading
number-plus-separator, so it took "1" as the track number and left
"01 Just Friends" as the title, leading zero and all. Fixed by
recursing: whatever's left after stripping one prefix is tried again,
and the innermost successful strip wins, so "1-01 Just Friends" now
correctly yields track number 1 (the real per-disc number) and a
clean title. Verified against the exact real filename pattern that
triggered it, plus a case that confirms a purely numeric *title*
("2112") doesn't get wrongly treated as a second track number.

**Verified:** `cargo build`/`test`/`fmt --check`/`clippy -D warnings`
all clean (224 lib + integration tests, 5 new: 2 for the escaping fix,
3 for the track-number fix). The escaping fix was independently
confirmed against a real running instance: tagged a real file with an
`&`-containing artist name, fetched the real Browse response, decoded
it, and confirmed a real XML parser now accepts it end to end.

---

## Phase 16 — Real audio properties on `<res>`

**Goal:** a track's real duration, bitrate, sample rate, bit depth,
and channel count appear on the DIDL-Lite `<res>` element, when
`lofty` could determine them.

Asked directly: "are track bitrates correctly being sent?" They
weren't — `<res>` only ever carried `protocolInfo` and `size`. Never a
deliberate scope decision, just never implemented. Verified against
primary sources before writing anything, not assumed: the real UPnP
ContentDirectory:1 spec's `res@bitrate` is bytes/second, not
bits/second (a real, common bug class in other implementations); the
real `lofty` 0.25.1 API exposes exactly this data via
`tagged_file.properties()` (`lofty::properties::FileProperties`),
parsed by default on the same read this project already does for tags
— no new dependency, no new binary-parsing surface.

- [x] `core::metadata_provider::Metadata` gained
      `duration_millis`/`bitrate`/`sample_rate`/`bits_per_sample`/
      `channels`, threaded through `index::TrackTags`/`Item`,
      `scanner.rs`, and `metadata::tag_cache`'s cached record, the same
      mechanical path every other tag field already takes.
      `FilenameMetadata` fills all five with `None`; `TagMetadata`
      computes them from the same opened `lofty` file it already reads
      for tags (a small refactor split `read_tag` into
      `open_tagged_file`/`primary_tag` so both the tag and
      `.properties()` come from one open, not two).
- [x] Milliseconds, not whole seconds: `lofty`'s `duration()` is
      always present (never `Option`), and truncating to whole seconds
      would show `0` — indistinguishable from "unknown" — for any
      track under a second. `duration_millis` is `None` only when
      `lofty` reports `Duration::ZERO` (its own signal it couldn't
      determine one); a real short duration is never treated as
      unknown. Rendered as the spec's own `H:MM:SS.mmm` grammar.
- [x] Bitrate uses `overall_bitrate` (the whole resource, matching the
      spec's literal wording), not `audio_bitrate` (audio-stream-only,
      excludes container overhead) — converted from `lofty`'s kbps to
      the DLNA bytes/sec convention once, in `metadata::tags`, so
      nothing downstream ever sees `lofty`'s own unit.
- [x] Every one of the five attributes is rendered independently:
      present when `lofty` determined it, cleanly absent (never a
      fabricated `0`) when it couldn't — confirmed for real MP3s,
      which have no fixed PCM bit depth, so `bitsPerSample` is
      correctly never emitted for them.

**Verified:** `cargo build`/`test`/`fmt --check`/`clippy -D warnings`
all clean (227 lib + integration tests). New tests confirm real sample
rate/channel count/bitrate from the project's existing minimal-MP3
fixture, a real non-zero `duration_millis` from a longer fixture (the
shared 30-frame fixture's true duration is under a second, too short
to prove the field populates at all), that all five round-trip
unchanged through a cache hit, and that `<res>` renders all five
correctly when known and omits all five cleanly when not. By hand
against a real running instance, cross-checked against `ffprobe` on
the exact same file: sample rate (44100), channels (2), and duration
(5.2125s vs. our `0:00:05.213`) matched almost exactly; bitrate (16000
vs. our 15875 bytes/sec) was within the expected ~1% variance between
two independent empirical bitrate estimators.

**Exit criterion:** a real file's Browse response carries accurate
duration/bitrate/sample rate/channel count on `<res>`, cross-checked
against an independent real tool. Met, verified above.

---

## After MVP

Not phased yet — pick these up only as real need shows up, per the
project's non-goals: client-specific quirk handling, real "Various
Artists" compilation bucketing, inotify-based instant
rescan.
