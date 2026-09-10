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

1. **Path resolution for file serving.** A request path (from a
   Browse-generated `<res>` URL, or a guessed one) has to resolve to a
   filesystem path that's verified to still be inside the configured media
   root, checked *after* canonicalization — reject `..`, encoded traversal
   (`%2e%2e`), and symlinks that escape the root. Implemented and fuzzed as
   a pure function (`&str -> Result<PathBuf, Error>`), no I/O side effects,
   so it's cheap to fuzz exhaustively.
2. **`Range` header parsing.** This is the exact bug class behind a real
   MiniDLNA CVE (a chunked-length parsing overflow). Malformed ranges get
   416, not a guess; start > end gets rejected; ranges past the file's
   length get rejected; all arithmetic on attacker-supplied offsets is
   checked (`checked_sub`/`checked_add`), never raw `-`/`+`.
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
   input. Fuzzed with `fuzz/fuzz_targets/ssdp_parse.rs` — 31M executions in
   a 30s local run, no crashes (see `docs/PLAN.md` Phase 2).

   **SOAP body parsing is implemented** (`src/core/soap.rs`,
   `parse_action`): an 8KB size cap checked twice — against the declared
   `Content-Length` before the body is even read (`core::http`), and
   again inside the parser itself, so a client that lies about
   `Content-Length` still can't get past the second check. Every XML
   operation returns `Result`/`Option`; the worst a hostile body can
   produce is a parse error or (for pathologically-nested-but-well-formed
   XML) a semantically wrong `SoapAction`, never a panic. Fuzzed with
   `fuzz/fuzz_targets/soap_parse.rs` — 3.4M executions in a 30s local run,
   no crashes.

   **The fail-closed contract on `ContentSource` is now attacker-reachable
   for real**, via `core::dispatch::browse` (Phase 5): a Browse request's
   `ObjectID` argument — arbitrary attacker-controlled text — becomes an
   `ObjectId` through `ObjectId::new`, which can't fail (it's just an
   opaque string wrapper), and is only checked for validity by the
   `ContentSource::children`/`entry` lookup, which returns `None` (mapped
   to UPnP fault 701 "No Such Object," not a panic or an out-of-bounds
   index) for anything not actually in the index. The object-ID
   *namespace* (multiple sources sharing "0", prefix-routing between them)
   still doesn't exist — that's Phase 8, and per `docs/DESIGN.md` it's
   `CompositeContentSource`'s job specifically, not something individual
   sources like `FolderMirror` need to know about.

Path resolution and Range parsing don't exist yet either — Phase 6.

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
  deliberate choice, not an oversight.
- A systemd unit with real hardening directives (`NoNewPrivileges`,
  `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, and friends) ships
  with the project — not yet written; tracked in Phase 9 of `PLAN.md`.

## What's explicitly not in scope

A malicious *file* on disk, used as an attack vector against the server
process itself, is not in the threat model. There's no transcoding in the
reference implementation, and tag/metadata reading (ID3, Vorbis comments)
is deferred entirely for MVP — Albums/Artists views use folder-structure
heuristics precisely so that deferral doesn't cost the feature. When tag
reading does land, it goes through a pure-Rust, `forbid(unsafe_code)`,
already-heavily-fuzzed library (`symphonia`), not hand-rolled binary
parsing. Until then, the server's attacker-reachable parsing surface is
deliberately narrow: network input only, the three paths above.
