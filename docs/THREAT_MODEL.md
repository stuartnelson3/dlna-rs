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

None of these three exist yet in code — they land in Phases 2, 5, and 6 of
`PLAN.md`. This document gets updated with real file/line references once
they do.

## Process-level hardening

- Never requires root. `server.port` above 1024 is enforced at config-load
  time (`ConfigError::PrivilegedPort`, in `src/config.rs`) — the binary
  should never need `CAP_NET_BIND_SERVICE`. SSDP's port 1900 is UDP;
  whether binding it needs any privilege on the target kernels is still an
  open question, tracked in Phase 2 of `PLAN.md`.
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
