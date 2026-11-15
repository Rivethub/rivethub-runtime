# Builder rules — RivetHub Rust runtime (one page; Phil signed off 2026-10-12)

You are building one crate of the Rust runtime. Everything you need: this page, CONTRACT.md (your crate's boundary rows),
ARCHITECTURE.md (where your crate sits and which ports it implements or calls), the inventory section and the recordings your
brief names. The TypeScript source at /opt/rivetos is REFERENCE for boundary behaviour only.

1. Behaviour at the boundary, nothing else. Your crate reproduces its contract rows on the recorded inputs. Internals are
   yours to design. Never copy the TypeScript's structure, file layout, workarounds or quirks. If you catch yourself porting a
   function, stop: ask what the boundary observes and implement that.
2. Idiomatic Rust, as if designing fresh. Enums over string tags and `match s.as_str()`; serde types over hand shape checks
   and untyped maps; ownership and `&mut` over copy-on-write spreads and whole-state clones; traits only where there are real
   alternatives (the kernel ports); standard crates over custom code (serde, chrono, url, std::path, axum, reqwest, sqlx,
   rusqlite, clap, tracing, thiserror); modules by responsibility; descriptive names (gateway, session, view — never den, room).
3. Depend only downward. Plug-ins are separate processes behind B17: the gateway's plug-in host
   supervises them and they reach rh only through the plug-in API socket with a scoped token. No plug-in code is loaded into
   rh: do not add dynamic loading, in-process registries or "capability" hooks for code that does not exist. Your crate imports `wire`, `config`, `kernel` (and `testkit` as a dev-dependency) plus external crates.
   Never another adapter or service. If you need another feature, you need a port: stop and write it in the notes.
4. Types are the validation. Deserialize into typed values; keep only the semantic checks the contract names (version equals
   1, non-negative counts, non-empty ids). No JavaScript emulation: no UTF-16 slicing, no Date.parse heuristics, no key-order
   preservation, no number-spelling rules, no `JsValue`/`JsString`/`JsNumber` — except the one capture event-id hash.
5. One runtime per process. Never build a tokio runtime in a library; never hold a `parking_lot` lock across `.await`;
   blocking work goes to `spawn_blocking`; long tasks take and honour a `CancellationToken`.
6. No comments of any kind. No `#[allow]`; a lint that is wrong for one spot takes `#[expect(lint, reason = "…")]`. No
   `unwrap`/`expect`/`panic!` in production code. No `unsafe`.
7. Tests prove the contract. Replay your recordings through `testkit` as ordinary `cargo test`s; port the TypeScript tests
   that test boundary behaviour; unit-test only logic the replays do not reach. Test files live in `tests/` by responsibility;
   small `#[cfg(test)]` modules only for private helpers; fixtures under `tests/fixtures/`, goldens copied under `tests/golden/`
   (never edited). No tests of code that does not exist. Measure coverage (`cargo llvm-cov -p <crate> --summary-only`) and keep
   it from dropping between rounds.
8. Size. Stay near the target in ARCHITECTURE.md; every source file under 600 lines. If you are far over, you are porting:
   stop and redesign.
9. Done means: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`,
   `cargo build --workspace`, `python3 scripts/gate.py .` all green; replays green; notes file
   written (decisions, deliberate deviations with contract evidence, coverage before/after); one Conventional Commit on your
   branch (`feat(rust): <crate> — <what>`), no trailers, no push, no rebase, no reset, no edits outside your crate except the
   workspace manifests.
10. CONTRACT.md is the boundary table and nothing else (Phil, 2026-11-14). Do not add notes, decisions or a record of your
   change to it. What a change does and why goes in its commit message and pull request; how to operate something goes in
   OPERATIONS.md or CLI.md. Edit CONTRACT.md only when a boundary row itself changes.
Privacy: the repository is public. Fixtures use /home/user, example.com, 00000000-0000-4000-8000-… ids, AcmeKit; never a name,
host or path from this lab. Never read /rivet-shared/wiki or memory dumps.
