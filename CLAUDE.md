# dlna-rs

A DLNA/UPnP music server in Rust. It scans a media directory, reads tags with `lofty`, and serves the library over SOAP and HTTP. See `SPEC.md` and `docs/DESIGN.md` for the design.

## Rules

- Work in small steps. Before you write code, state the plan in 2-3 lines.
- Put business rules in pure functions and test them directly. Keep I/O at the edges. Grouping logic in `src/content/music_library.rs` works on an index in memory, so test it without files or a network.
- Write or update a test with each change. Run `cargo test` after each edit.
- Before you commit, run `cargo fmt`, `cargo clippy --all-targets` and `cargo test`. All three must pass.
- Reuse the test helpers in the module you change, for example `tagged_item` and `titles` in `music_library.rs`, or `fixture_with` in `tags.rs`. Add new helpers next to them.
- Fix a bug with a test that fails first. Use the real tags or the real folder shape that caused it.
- Test with the real shape of the data. The scanner puts each configured media directory under the index root. A test tree without that layer can hide a bug.
- Investigate with real data before you decide on a cause. Read the real tags and browse the real server. Do not guess from the code alone.
- Do not add dependencies without asking. `deny.toml` limits licenses and sources.
- Call out edge cases and assumptions. Do not choose silently. Tags, Unicode and shared index state matter most here.
- Keep every `ObjectID` pure ASCII. The real client, PlayFi, changes non-ASCII text in an id.
- Add a field to the tag cache only with a schema bump. Old entries must be read again.
- Use the `simple-made-easy` skill when you design, write, refactor or review code.
- Use the `simple-english` skill for prose that a person will read: comments, commit messages, PR bodies, docs and chat replies.

## Commits

- One fix per commit. Each commit has its test.
- End the message with the `Co-Authored-By` line from the session.
- Do not push or release unless asked.

## Live checks

- The user deploys the binary by hand. Compare the `sha256` of the running binary with your build before you trust a live test.
- `examples/verify_music_library.rs` walks a running server and checks counts and parent ids.
