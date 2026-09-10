# Threat model

This is a living document. Update it in the same PR that changes the
security-relevant behavior it describes.

## Trust boundary

The LAN is semi-trusted at best. Any device on it — a compromised IoT
gadget, a guest on the WiFi, a malicious "smart" TV — can reach SSDP, the
SOAP endpoint, and the HTTP file server. All three are untrusted-input
surfaces, full stop.

`dlna-rs` is not meant to be reachable from the internet. There's no
account system and no cross-network auth story, because DLNA doesn't have
one either and inventing one wouldn't fit the protocol clients actually
speak. LAN-only exposure **is** the security boundary. Don't port-forward
this.

## The three attacker-reachable code paths that matter most

These get the most test and fuzz investment, because they're the ones a
LAN attacker can actually hit.

1. **Path resolution for file serving.** The classic version of this bug
   (a request path with `..`/encoded traversal getting concatenated onto
   a filesystem root) doesn't have a way into this codebase: item URLs
   are `/item/{id}` (decided in Phase 5) where `{id}` is an opaque
   `ObjectId` that only ever drives an index lookup — no attacker string
   is ever built into a path. `core::http::router::parse_item_path`
   (extracting `{id}` from the URL) is still fuzzed
   (`fuzz/fuzz_targets/path_resolve.rs`) as the one place attacker text
   gets parsed at all, pure and total, no I/O. The *real* residual risk
   here is different: `follow_symlinks` letting something inside the
   configured root resolve to a target outside it. That's handled by
   `core::http::resolve_within_roots`, which canonicalizes an item's path
   at serve time (not just scan time — a symlink's target can change
   between scans) and checks it against the canonicalized configured
   roots. Property-tested with real tempdir fixtures, including symlinks
   that both do and don't escape the root, and a regression guard on the
   classic naive-string-prefix trap (`/media/music-private` must not look
   like it's inside `/media/music`). Fuzzed with
   `fuzz/fuzz_targets/path_resolve.rs` — 122M executions in a 120s local
   run (Phase 9), no crashes.
2. **`Range` header parsing.** Implemented (`core::http::range`), and this
   is the exact bug class behind a real MiniDLNA CVE (a chunked-length
   parsing overflow). Malformed ranges get 416, not a guess; start > end
   gets rejected; ranges past the file's length get rejected; any range
   at all against an empty file gets rejected; all arithmetic on
   attacker-supplied offsets is checked or saturating
   (`checked_sub`/`saturating_sub`), never raw `-`/`+`. Fuzzed
   (`fuzz/fuzz_targets/range_parse.rs`) against both the header string and
   the file size it's checked against — 121M executions in a 120s local
   run (Phase 9), no crashes.
3. **SSDP datagram parsing and SOAP body parsing**, including the
   object-ID namespace that routes a Browse request to a `ContentSource`.
   Both are reachable from any device on the LAN. Buffer sizes are bounded
   explicitly (a SOAP body over a few KB gets rejected before parsing —
   real ContentDirectory requests are tiny), neither parser panics on
   malformed input, and both get dedicated `cargo-fuzz` targets. An
   `ObjectID` that doesn't match any `ContentSource`'s prefix fails closed
   (not-found), never a panic or an out-of-bounds index.

   **SSDP parsing is implemented** (`src/core/ssdp/message.rs`,
   `parse_search_request`): a hard 2KB size cap before any parsing happens,
   rejects anything that isn't valid UTF-8 or a well-formed M-SEARCH
   request-line, returns `Result` rather than panicking on any malformed
   input. Fuzzed with `fuzz/fuzz_targets/ssdp_parse.rs` — 118M executions
   in a 120s local run (Phase 9), no crashes.

   **SOAP body parsing is implemented** (`src/core/soap.rs`,
   `parse_action`): an 8KB size cap checked twice — against the declared
   `Content-Length` before the body is even read (`core::http`), and
   again inside the parser itself, so a client that lies about
   `Content-Length` still can't get past the second check. Every XML
   operation returns `Result`/`Option`; the worst a hostile body can
   produce is a parse error or (for pathologically-nested-but-well-formed
   XML) a semantically wrong `SoapAction`, never a panic. Fuzzed with
   `fuzz/fuzz_targets/soap_parse.rs` — 5.4M executions in a 120s local run
   (Phase 9), no crashes. The lower rate against the other three targets
   matches XML parsing costing more per input than plain string parsing,
   not a weaker fuzz session.

   **The fail-closed contract on `ContentSource` is attacker-reachable for
   real**, via `core::dispatch::browse` (Phase 5): a Browse request's
   `ObjectID` argument — arbitrary attacker-controlled text — becomes an
   `ObjectId` through `ObjectId::new`, which can't fail (it's just an
   opaque string wrapper), and is only checked for validity by the
   `ContentSource::children`/`entry` lookup, which returns `None` (mapped
   to UPnP fault 701 "No Such Object," not a panic or an out-of-bounds
   index) for anything not actually in the index.

   **The object-ID namespace is implemented** (Phase 8,
   `content::composite::CompositeContentSource`): every configured view
   mounts under its own prefix, joined to the real ID with `$`
   (`"albums$42"`). A Browse call splits on the first `$`, looks up the
   matching mount, and fails closed — `None`, UPnP fault 701 — for either
   an unrecognized prefix or a recognized prefix paired with an ID its
   mount doesn't know. An individual source like `FolderMirror` or
   `MusicLibraryView` never sees a prefix at all; only
   `CompositeContentSource` adds and strips them, exactly as this
   document originally called for. Verified by hand against a real
   running instance: an unknown prefix (`"bogus$0"`) and a known prefix
   with an unknown ID (`"albums$99999"`) both returned fault 701, and a
   real Browse round-trip through a live mount returned real content
   (`docs/PLAN.md` Phase 8 has the full verification log).

## Process-level hardening

- Never requires root. `server.port` above 1024 is enforced at config-load
  time (`ConfigError::PrivilegedPort`, in `src/config.rs`) — the binary
  should never need `CAP_NET_BIND_SERVICE`. SSDP's port 1900 is UDP;
  confirmed empirically (Phase 2) that binding it and joining the
  multicast group need no elevated privilege on Linux for an ordinary
  user process.
- `#![forbid(unsafe_code)]` at the crate root (`src/main.rs`). Dependencies
  are audited separately, not held to the same bar — a `cargo geiger` pass
  is a periodic check on what unsafe exists in the dependency tree, not a
  hard CI gate (too noisy to gate on).
- Release profile uses `panic = "unwind"`, not `"abort"` (see
  `Cargo.toml`). With tokio's task-per-connection model, a panic inside one
  request-handling task fails only that task. `panic = "abort"` would turn
  any single reachable panic — including one triggered by a malformed
  request from an untrusted LAN client — into a full-process crash. That's
  a real availability risk for a network-facing daemon, so it's a
  deliberate choice, not an oversight. Proven, not just reasoned about:
  `core::http::tests::a_panic_in_one_connection_task_does_not_take_down_the_server`
  (Phase 9) binds a real server, panics one connection deliberately, and
  confirms a second connection still gets a normal response.
- A systemd unit with real hardening directives ships with the project:
  `systemd/dlna-rs.service`. Every sandboxing directive in it was tested
  against a real running instance under `systemd-run --user`, including
  a negative control that proved `RestrictAddressFamilies` needs
  `AF_NETLINK` (interface resolution fails loudly without it, not
  silently). See Phase 9 of `PLAN.md` for the full verification log,
  and the unit file's own comments for the `DynamicUser` vs. static-user
  decision.

## What's explicitly not in scope

A malicious *file* on disk, used as an attack vector against the server
process itself, is not in the threat model. There's no transcoding in the
reference implementation. Real tag reading (ID3, Vorbis comments) and
embedded cover art landed in Phase 12, using `lofty`, not the
`symphonia` this document originally named. See `docs/PLAN.md`'s
Phase 12 section for the comparison that changed it. Tag reading parses
only files the operator already placed under a configured media
directory: config-provenance input, the same category the Phase 7
rescan timer's own re-walk already covers, not anything reachable from
the network. Every `lofty` call in `metadata::tags` is wrapped in error
handling that logs a parse failure and returns empty fields rather than
propagating it, so a single malformed tag block can't abort a whole
scan. Someone with write access to the configured media directory
already has more direct ways to cause harm than a crafted tag block, so
that access level itself stays out of scope, same as it always has.
Albums/Artists grouping still uses folder-structure heuristics, not
tags: that choice was about avoiding two disagreeing grouping
mechanisms (see `docs/PLAN.md` Phase 8), not about deferring tag
reading, which is why adding it in Phase 12 didn't need to touch
grouping at all. The server's attacker-reachable parsing surface stays
deliberately narrow: network input only, the three paths above. Tag
reading doesn't widen it, since it never touches network input.

Phase 13's external-cover-art-file lookup (`metadata::cover_files`)
reads the same config-provenance category as tag reading: image files
the operator already placed under a configured media directory, never
network input. Its one filesystem-boundary property — checking a
track's own directory, and for a multi-disc album the directory above
it, without ever stepping outside a configured media directory — is
verified directly, not just assumed: `TagMetadata` carries the same
canonicalized media roots `core::http::HttpServer` does, and a unit
test proves the lookup refuses a cover file sitting just outside a
configured root even when a plain "check the parent directory" search
would have found it.

The Phase 7 rescan timer doesn't change this. It re-walks the *configured*
media directories on a schedule — config-provenance paths the operator
chose, not anything derived from network input — using the same scanner
code path as the initial scan. No new attacker-reachable surface, per the
spec's own reasoning for choosing a timer over inotify.
