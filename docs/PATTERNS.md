# Patterns

Every entry is an anti-pattern found in review, the pattern to use instead, and the check that enforces it. A clippy, Dylint,
cargo-deny or `cargo xtask` finding names its entry. To add one: write the entry, add the enforcer (a `[workspace.lints]`
entry, a `clippy.toml` disallowed entry, a Dylint lint in `lints/`, a `deny.toml` ban, or an `xtask` check), and add a firing
and a passing case to the Dylint UI tests or the `xtask/tests/` repositories. `cargo xtask check` fails if an entry names no
known lint or check.

## COMMENT
Anti-pattern: comments explaining code. Pattern: names and types carry the meaning; a non-obvious constraint becomes a type
or a test. Check: xtask `COMMENT` (a real Rust lexer over `crates/**.rs`, comments in tests included).

## UTF8
Anti-pattern: a `.rs` file under `crates/` that is not valid UTF-8 — the Rust lexer cannot read it, so every other scan
silently skips it. Pattern: valid UTF-8 source; byte payloads live in fixtures with an explicit encoding.
Check: xtask `UTF8` (the COMMENT read turns the decode failure into a finding).

## SLEEP
Anti-pattern: wall-clock synchronization — `std::thread::sleep` anywhere, or `tokio::time::sleep` /
`tokio::time::sleep_until` inside a test. Production timers (heartbeat, deadlines) legitimately sleep; a test that sleeps
is racing the scheduler and breaks under a paused or loaded clock. Pattern: tests await the event with a timeout
(`tokio::time::timeout`, channels, watch); a genuine timing test (the wait is the fixture — an idle window, an escalation
grace) keeps the sleep with `#[expect(clippy::disallowed_methods, reason = "…")]` and, for the tokio timers,
`#[cfg_attr(dylint_lib = "rr_lints", expect(sleep, reason = "…"))]` on the statement.
Check: Clippy `disallowed-methods` (`std::thread::sleep`); dylint `sleep` (tokio sleep in test code only).

## ENV
Anti-pattern: reading the environment anywhere but configuration — `std::env::var`, `var_os`, `vars`, `vars_os`,
`temp_dir`, `home_dir`, `current_dir`. Pattern: the binary builds `config::Environment` from `std::env::vars_os()` once
and passes `Config` down; values arrive typed. `config` itself and the `rivethub` binary's `main.rs` are the only readers.
Check: xtask `ENV` (over every tracked `.rs` file, tests included, exempting `crates/config/` and
`crates/rivethub/src/main.rs`); Clippy `disallowed-methods` (`std::env::var`, `var_os`) as the global backstop.

## PRIVILEGE
Anti-pattern: a fail-open privilege default — `unwrap_or(…Viewer::System…)`, `unwrap_or(…::Admin…)`,
`unwrap_or(…::Owner…)`, or `unwrap_or_default()` on a `*Viewer`/`*Role`/`*Permission` value. Absence is a refusal, not a
privilege: an unread role is an error (or an explicit least-privilege constant written on the line), never the widest
default. Pattern: propagate with `?`; a sanctioned default is spelled out at the call site with a named constant.
Check: xtask `PRIVILEGE`.

## ALLOW
Anti-pattern: `#[allow(...)]`, or `#[expect(...)]` without a reason, to silence a lint. Pattern: fix the code. Where a lint
is genuinely wrong for one spot, `#[expect(lint, reason = "…")]` — the compiler re-checks it every build, so it cannot go
stale. A Dylint lint wrong for one spot takes `#[cfg_attr(dylint_lib = "rr_lints", expect(<lint>, reason = "…"))]` — the
`dylint_lib` cfg keeps the unknown lint name off the plain rustc/Clippy legs, and `[workspace.lints.rust]` declares the cfg
once so `unexpected_cfgs` stays quiet (no per-crate `build.rs`). A lint wrong for the whole workspace is a reviewed change
to `[workspace.lints]`. A file-wide waiver is not a spot: `#![expect(...)]` / `#![allow(...)]` are findings (see WAIVER).
Check: Clippy `allow_attributes` and `allow_attributes_without_reason`; xtask `WAIVER`.

## UNWRAP
Anti-pattern: `.unwrap()` / `.expect()` in production code. Pattern: return the crate's `thiserror` error with `?`; a value
that cannot be absent is made non-optional by its type. Check: clippy `unwrap_used`, `expect_used` (tests exempt via
`clippy.toml`).

## FALLBACK
Anti-pattern: a fallback that hides a decision — `unwrap_or_default()` / `unwrap_or(0)` on a value that failed to parse or
decode, `let _ = …` on a `Result`, `let Ok(..) = .. else`, or a trailing `.ok()`. The value becomes a default, `None` or a
discarded result and no reader can tell whether anyone decided that. Pattern: every site is one of three things, and the
types say which. (1) An error: propagate it with `?` into the crate's `thiserror` error. (2) A recorded rule: the inventory
or a recording says what happens (the hook never fails the harness, so the log write is best effort; a non-UTF-8 header is
no header) — keep the behaviour, name it (`fn best_effort_log(..)`, `fn header_text(..) -> Option<&str>`) and pin it with a
test. (3) A real absence: the domain has no value — keep the `Option` to the one place that uses it instead of defaulting
early. A discard that is genuinely the last write (a final stderr report) takes `#[expect(clippy::let_underscore_must_use,
reason = "…")]`, never a bare `let _ =`. Check: Clippy `let_underscore_must_use`, `let_underscore_untyped` (a discarded
`Result` or `#[must_use]` value cannot land); Dylint `fallback` (`FALLBACK`) uses the receiver type to flag `Result::ok`, `unwrap_or*` and `map_or*`,
error match arms producing values or nothing, and `let Ok(..) = .. else` returning a default or skipping work.
`Option::unwrap_or` with a meaningful value is legal; sentinel literals (`""`, `"unknown"`, `"default"`, `0`, `-1`)
are findings. Only a statement-level `#[cfg_attr(dylint_lib = "rr_lints", expect(fallback, reason = "…"))]`
citing a CONTRACT row or PATTERNS id can waive a finding. Function names do not waive findings. Dylint
`fallback_waiver` rejects missing citations and item-wide expectations. Tests remain exempt from FALLBACK.
Existing hits live in `xtask/baseline.toml` (see BASELINE) and shrink only.

## UNSAFE
Anti-pattern: `unsafe`. Pattern: a safe crate API. Check: rustc `unsafe_code = "forbid"` in `[workspace.lints]`.

## JSMODEL
Anti-pattern: emulating JavaScript values or built-ins (`JsNumber`, `JsString`, `serde_json::Number` for "any number",
UTF-16 slicing, `Date.parse` heuristics, key-order preservation, accepting `"132"` or `10.7` where an integer is meant).
Pattern: plain Rust types (`i64` epoch ms, `f64`, `u16`); the contract defines what is observable, and JavaScript runtime
quirks on inputs RivetHub clients never send are not part of it (CONTRACT §2) — such recorded exchanges are listed as
deviations, not reproduced. This includes JavaScript semantics, not only types: truthiness (`is_truthy`), `typeof` names,
`"[object Object]"` display, `Object.keys` over a string, a hand-rolled JavaScript-regex checker, evaluating JavaScript
expressions found in recordings (`JSON.stringify(...)`, `i%2?a:b`) — build such bodies in Rust in the test that needs them. A
recording generated by fuzzing a JavaScript function (wrong types, falsy values) is matched exactly only where the input is
well-typed; for type-confused input the Rust outcome is serde's typed error, and the replay asserts only that the input is
rejected. The names follow the same rule: no function prefixed `js_` (`js_number`, `js_stringify`) and no type matching
`Js[A-Z]` (`JsJson`); the one exception is the capture event-id hash module, whose order-keeping JSON model is the
contract's named deviation. Check: clippy `disallowed-types` (`serde_json::Number`); dylint `jsmodel` (Js* types,
truthy/typeof/NumberOr names, `[object Object]` and `JSON.stringify` strings); xtask `JSMODEL` (`js_`-prefixed functions
and `Js[A-Z]` types, exempting the capture event-id hash module, with existing hits in `xtask/baseline.toml`).

## UNTYPED
Anti-pattern: `serde_json::Value` / `Map<String, Value>` in structs, helpers or signatures — including private fields with
accessors and deserialize helpers that return a raw map — or a `serde_json::json!(…)` invocation building request bodies and
RPC frames. Pattern: a serde struct or enum per payload (or the crate's own protocol types; a value the contract defines as
arbitrary is built through the format crate's own type or a typed struct serialized with `serde_json::to_value`, never
`json!`). Two named types in `crates/values/src/json.rs` are the only holders of free-form JSON: `values::Extra` for unknown
fields the contract preserves (`#[serde(flatten, default)] pub extra: Extra`), and `values::JsonObject` for values the
contract defines as arbitrary JSON (MCP tool-call arguments, tool input schemas). Nothing else names
`serde_json::Value` or `Map`. The same holds for `serde_yaml::Value`/`Mapping`/`Number`: only `crates/config/src/load.rs`
and `resolve.rs` touch the raw document (parse, `${VAR}` substitution over string leaves, `serde_yaml::from_value`).
Check: dylint `untyped` (serde_json and serde_yaml, the `json!` macro call site read through the expansion —
`ExpnKind::Macro` with the `serde_json::json` def path — and `IgnoredAny`, none of which clippy's `disallowed-types` can
carry, with the exemptions clippy cannot express: the per-file production exemptions `values/src/json.rs` and config
`load.rs`/`resolve.rs`; test code in `wire` and `testkit` only — the crates whose job is structural JSON
comparison, attributed to the package by file path so a test target compiled under its own crate name is not mistaken for a
third crate — and `testkit` wholesale, where every other production rule still applies to `testkit/src`).

## TAGFIELD
Anti-pattern: a constant discriminator stored as a field, `pub r#type: SnapshotTag` with a one-variant enum. Pattern:
`#[serde(tag = "type", rename = "snapshot")]` on the struct, or `#[serde(tag = "type")]` on the enum of variants. A `type`
field whose enum has several values and whose variants share the same fields is fine. Check: dylint `tagfield`.

## STRINGLY
Anti-pattern: a `String` field whose values are a closed set that already has an enum (`harness: Option<String>` while
`HarnessId` exists). Pattern: use the enum. Check: dylint `stringly` (a field named after a workspace enum that names a
closed category — `…Id`, `…Kind`, `…Status`, `…Role`, `…Phase`, `…Mode`, `…State`, `…Source`, `…Activity`; e.g. `harness_id`
or `harness` for `HarnessId` — must not be `String`; the scan sees the local crate's and the workspace crates' enums).

## JSONMAP
Anti-pattern: `serde_json::Map<String, T>` as a typed map. Pattern: `BTreeMap<K, T>` (ordered output) or `HashMap<K, T>`,
with a typed key (`SessionId`) where one exists. Check: dylint `untyped` (any `serde_json::Map`; the UNTYPED exemptions
apply, so test code is exempt only in `wire` and `testkit`).

## DISPATCH
Anti-pattern: `match s.as_str() { "a" => … }` on a protocol tag or kind. Pattern: a serde enum
(`#[serde(tag = "type", rename_all = "snake_case")]`) and `match` on the variant. Check: dylint `dispatch`.

## NAMING
Anti-pattern: `den` / `room` identifiers inherited from the TypeScript. Pattern: descriptive names (`gateway`, `session`,
`view`). Wire strings and legacy keys that must stay (`rooms`, the deprecated `den:` config alias) are serde renames or string
constants, never Rust identifiers, modules or file names. Check: dylint `naming` (identifier words on snake/camel
boundaries, method calls included; strings and serde renames never match — `golden_path` and `NetworkGoldenPath` pass);
xtask `NAMING` (file and directory names).

## RUNTIME
Anti-pattern: a library building a tokio runtime or using `#[tokio::main]`. Pattern: libraries expose `async fn`; only the
`rivethub` package's CLI builds the runtime (`rh`, and the one-release `rivetos` alias, both from `main.rs`). Check:
dylint `runtime` (a runtime build — `Builder::new_multi_thread`, `new_current_thread`, `Runtime::new`, `#[tokio::main]` —
outside test code and outside those composition-root binaries).

## STDLOCK
Anti-pattern: `std::sync::Mutex` / `RwLock`. Pattern: `parking_lot` for short critical sections never held across `.await`;
`tokio::sync` when state is held across an await. Check: clippy `disallowed-types`.

## PRINT
Anti-pattern: `println!` / `eprintln!` in libraries. Pattern: `tracing` events; only `cli` and `rivethub` write to stdout.
Check: clippy `print_stdout`, `print_stderr`; the sanctioned stdout writers in `cli` and `rivethub`
carry `#[expect(lint, reason = "…")]` when they land.

## STRSET
Anti-pattern: `const`/`static` tables of protocol strings — lists of known provider types, of types that accept a base
URL, of known keys per section — re-encoding one closed set several times. Pattern: one enum
(`#[derive(Deserialize)] #[serde(rename_all = "kebab-case")] enum ProviderType`) with methods for each property
(`is_cli()`, `takes_base_url()`), and unknown keys detected by serde (see UNKNOWNKEYS), never by a key list. Messages that
list allowed values are built from the enum. Check: dylint `strset`.

## DYNVALUE
Anti-pattern: a home-made dynamic value tree (`enum Node { Null, Bool, Int, Str, Seq, Map }`, an order-preserving
`Mapping`, a hand-written `Deserializer` for it) with validation as lookups over that tree. Pattern: deserialize straight
into typed structs and enums; validate the typed value. When a pass genuinely needs the raw tree (the `${VAR}` substitution
over string leaves before typing), use the format crate's own value type (`serde_yaml::Value`) inside that one function and
convert with `serde_yaml::from_value`; never define a value model. Check: dylint `dynvalue` (a `Null` variant, a hand-written
`Deserializer` impl).

## UNKNOWNKEYS
Anti-pattern: detecting unknown keys with `#[serde(flatten)] unknown: BTreeMap<String, IgnoredAny>` (a zero-sized map that
clippy rejects) or with `Option<IgnoredAny>` placeholder fields that list the known keys. Pattern: plain typed structs, read
through `serde_ignored::deserialize(deserializer, |path| ...)`; the callback receives every ignored key with its full path,
and the warning text is chosen from that path. Check: dylint `untyped` (`IgnoredAny`).

## TYPEERR
Anti-pattern: a tri-state wrapper (`Field<T> { Missing, Set(T), Wrong(raw value) }`) with a hand-written `Deserialize` so
every field can report its own wrong-type message. Pattern: fields are `T` / `Option<T>` / enums; deserialize with
`serde_path_to_error::deserialize`, and a wrong type becomes one error finding at the reported path. Semantic checks
(ranges, required sections, cross-references) run over the typed value afterwards and keep their verbatim messages.
Check: dylint `customserde` and `untyped` (the wrapper needs both).

## OPAQUE
Anti-pattern: an empty struct (`pub struct OpaqueSection {}`) that accepts a section and throws its contents away, so the
setting never reaches the code that needs it. Pattern: type the section from the inventory; a removed key is not a field
at all (`serde_ignored` reports it, and the error text comes from an enum of removed paths); a value the contract defines
as arbitrary uses the sanctioned holder (`values::JsonObject`). Check: dylint `opaque`.

## TYPEMACRO
Anti-pattern: `macro_rules!` that stamps out structs, enums or their impls (`closed!(Effort { Low, High })`,
`provider_entry!(...)`) to save derive lines. The types disappear from grep, docs and the reader. Pattern: write each type
out with its derives; share fields with `#[serde(flatten)]` of a common struct. Check: dylint `typemacro`.

## REDUNDANTRENAME
Anti-pattern: `#[serde(rename = "max_tokens")] pub max_tokens`. Pattern: no attribute when the names match; `rename_all`
on the container when a whole struct uses another case; `alias` for an accepted second spelling (`agentId`), never a
second field. Check: dylint `redundant_rename`.

## NULLABLE
Anti-pattern: `Option<Option<T>>` (with `serde_with::rust::double_option`) to reproduce whether a recorded server sent
`null` or left a field out. Clients treat both the same. Pattern: `Option<T>` with `skip_serializing_if`; roundtrip tests
compare JSON with null-valued object members removed on both sides. `Option<Option<T>>` is only for request bodies where
the contract gives `null` its own meaning ("clear this field"), and such a type is named `…Patch` or `…Update`.
Check: clippy `option_option`; dylint `nullable` (the `*Patch`/`*Update` carve-out clippy cannot express).

## LANGUAGES
Anti-pattern: a second language in the runtime repo — TypeScript or JavaScript hook kits, Python plugins, Node installers,
`package.json` — or tooling written in another language because it was quicker. Pattern: the repo is Rust (crates and a
`cargo xtask` for repo tooling); the git hooks are the xtask binary itself, installed into `target/git-hooks/` by
`cargo xtask install-hooks` and dispatching on the name they are invoked as. Clients of the runtime, including the
per-harness capture hooks, live in their own repos and talk to the runtime over its boundaries. Check: xtask `LANGUAGES`
(an allow-list in `xtask/languages.toml` over every tracked file: `*.rs`, `*.toml`, `Cargo.lock`, `*.md`, `LICENSE`,
dotfiles, `.github/`, recorded data (JSON, JSONL, Markdown, text) only under `crates/*/tests/golden/` or
`crates/*/tests/fixtures/` and the `xtask/data/` hash files, `coverage-floor.txt`).

## ENV
Anti-pattern: reading the environment anywhere but configuration. Pattern: the binary builds `config::Environment` from
`std::env::vars_os()` once and passes `Config` down. Check: xtask `ENV` (every tracked `.rs` file, tests included;
`crates/config/` and the `rivethub` binary's `main.rs` are the only exemptions); clippy `disallowed-methods`
(`std::env::var`, `var_os`).

## EXIT
Anti-pattern: `std::process::exit` outside `main`. Pattern: return an error; the binary maps it to the exit code.
Check: clippy `exit`, `disallowed-methods`.

## DEPS
Anti-pattern: a crate outside the policy (OpenSSL, `anyhow`, a second async runtime, a tmux binding), `once_cell`/`lazy_static`
as direct dependencies, or a dependency declared in a manifest the crate never uses. Pattern: rustls; `thiserror`;
`std::sync::LazyLock`/`OnceLock`; tokio; a dependency lands in the manifest with the commit that uses it and is
removed when its last use goes (never excused with a `cargo-machete` ignore list). Check: the DEPS
policy in `deny.toml` bans for the first group; `once_cell`/`lazy_static` must not be direct dependencies and is checked
in review (other crates pull them transitively, so they are not banned in `deny.toml`); `cargo machete` in
`cargo xtask check` fails the gate on an unused direct dependency.

## TLS
Anti-pattern: two rustls crypto providers in one build (a dependency enabling rustls `ring` while the code assumes another provider), or a rustls config built through the process-level default (`ServerConfig::builder()`, `ClientConfig::builder()`, `WebPkiClientVerifier::builder()`) — with both providers compiled in, rustls cannot pick a process default and the builder panics. Pattern: one provider for the whole workspace, `aws-lc-rs` (the rustls and reqwest default): `rustls` and `tokio-rustls` carry the `aws-lc-rs` feature (never `ring`), `rcgen` (dev) its `aws_lc_rs` feature, sqlx the `tls-rustls-aws-lc-rs` variant; every `ServerConfig`, `ClientConfig` and client-cert verifier names the provider explicitly —
`ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider())).with_safe_default_protocol_versions()?`, the same builder chain for any `ClientConfig`, `WebPkiClientVerifier::builder_with_provider(roots, provider)` — never the process-level default. Check: the cargo-deny `ring` ban.

## LINTPOLICY
Anti-pattern: a lane editing the lint policy to get its own code through. Pattern: fix the code; a lint policy change is
its own reviewed commit. Check: xtask `LINTPOLICY` (`[workspace.lints]` equals `xtask/lints-policy.toml`, the file the
policy lane edits).

## MANUALDERIVE
Anti-pattern: a hand-written `impl PartialEq/Clone/Debug/Default/Hash/Ord for` that does what `#[derive]` does. Pattern:
`#[derive(...)]`. Check: clippy `derivable_impls`; dylint `manualderive` (every derivable trait).

## CUSTOMSERDE
Anti-pattern: hand-written `impl Serialize`/`impl Deserialize`, or `#[serde(with = "crate::…")]` helper modules. Pattern:
derives; `#[serde(try_from = "String", into = "String")]` with `TryFrom`/`From` for validated newtypes (`SessionId`,
`ProtocolVersion` via `try_from = "u8"`); `strum` for string forms of enums; `serde_with` (e.g.
`with = "::serde_with::rust::double_option"` where the contract distinguishes absent from `null`). Check: dylint
`customserde`.

## CONTRACT
Anti-pattern: a public serde type in `wire` without `schemars::JsonSchema`, or a type in a module the contract covers that
is missing from `ROOTS`, so clients hand-write the DTO and drift (Android's harness list without `cowork`). Pattern: every
public serde type in `wire` derives `schemars::JsonSchema`; every type in a contract module is a root in
`crates/wire/src/contract.rs`; `contract/sent.schema.json` (what the runtime sends) and `contract/accepted.schema.json`
(what it accepts) are generated from those roots and committed, and clients generate their types from them. Two types
with one schema name get a `#[schemars(rename)]`. Check: xtask `CONTRACT`; the `wire` test `contract` regenerates a stale
schema and fails.

## DUPNAMES
Anti-pattern: a `match` table mapping each variant to its string (`Self::ClaudeCode => "claude-code"`) beside serde's own
renames, plus `FromStr` that round-trips through a deserializer. Pattern: one naming rule on the enum —
`#[derive(Serialize, Deserialize, strum::Display, strum::EnumString)]` with `#[serde(rename_all = "kebab-case")]` and
`#[strum(serialize_all = "kebab-case")]`. Check: dylint `dupnames` (three or more `Self::X => "…"` arms in a file).

## COUNTTYPE
Anti-pattern: JavaScript-number habits in integer fields — `port: i64`, `limit: i64`, token counts as `i64`. Pattern:
`u16` for ports, `u32`/`u64`/`usize` for counts and limits; `i64` only for signed quantities and epoch milliseconds; `u32`
for a pid. Settings that are durations, sizes or counts (`timeout_ms`, `embed_expected_dims`, quiet-hour `start`) are
unsigned integers, never `f64`; a fractional value in a config file is a type error. Measured values a server reports
(`prompt_ms: 12.4`) stay `f64`, as do sampling weights (`temperature`, `top_p`); `top_k` is an integer (`i32`, -1 disables).
Check: dylint `counttype` (ports, counts, pids everywhere; the float settings rule in `config`).

## ROWGET
Anti-pattern: `sqlx::Row::get` on a query result — it panics on a missing column or a type mismatch, so a drifted column or
a changed column width is a panic in the request path, and a fallback chained onto it silently substitutes a value.
Pattern: every query reads into a typed row: `#[derive(sqlx::FromRow)]` with `QueryBuilder::build_query_as` where the
statement is dynamic and `query_as!` where it is static, so a mismatch is a typed error; each arm's SQL casts derived
columns to one declared type instead of the reader negotiating widths; `try_get` with `?` for the odd single column, and a
decode failure is an error, never a substituted value. Check: clippy `disallowed-methods` (`sqlx::Row::get`); xtask
`SQLOFFLINE` (every cargo command in `cargo xtask check` runs with `SQLX_OFFLINE=true`, so a stale `.sqlx/` fails the local
gate exactly as it fails CI, and `cargo sqlx prepare --workspace --check` runs when `DATABASE_URL` is set).

## DUPTYPE
Anti-pattern: the same enum defined in several modules (`Role`, `CaptureRole`, `stream::Role`). Pattern: one definition,
imported where needed; a wire payload that genuinely differs gets a type that differs. Check: dylint `duptype` (same
variant set and the same final name word, e.g. `Role`/`CaptureRole`, across the crate and its dependencies — renaming
must not hide it). A kernel enum that mirrors a `wire` contract enum is the one sanctioned twin; the kernel does not
depend on `wire` (see KERNELWIRE), so the lint never sees the pair.

## DUPFIXTURE
Anti-pattern: the same recording vendored by more than one crate (`capture/writer/*` under `crates/wire/tests/golden/`
and again under `crates/testkit/tests/fixtures/`); the copies drift when a re-recording updates one and not the other.
Pattern: a recording shared by more than one crate lives once, under `crates/testkit/tests/fixtures/`, and every crate
reads it through testkit's path helper (`testkit::recording`); a recording only one crate uses stays in that crate, and
identical files inside one crate (separate scenarios that recorded the same bytes, e.g. provider chunks) are fine.
Check: xtask `DUPFIXTURE` (a tracked file under any `crates/*/tests/golden/` or `crates/*/tests/fixtures/` whose bytes
equal a file in a different crate's fixture tree is a finding naming both paths).

## LABPATH
Anti-pattern: a repository file reading or naming the lab plans tree (`/rivet-shared/plans/…`) — tests that read
recordings from the share pass only on the lab machine and fail on a CI runner with NotFound; the plans tree is the
lab's, never the repository's. Pattern: a recording shared by crates is vendored once under
`crates/testkit/tests/fixtures/` (see DUPFIXTURE) and read through testkit's path helper (`testkit::recording`);
`/rivet-shared` on its own stays legal — it is a product default (config's shared dir). Recorded
data under `crates/*/tests/golden/` and `crates/*/tests/fixtures/` is exempt: a recording keeps its recorded bytes,
including note fields that quote the lab as recorded. Check: xtask `LABPATH` (a tracked file under `crates/` or
`xtask/` containing `/rivet-shared/plans/`).

## WIREPUB
Anti-pattern: private fields with accessor methods on `wire` payload structs. Pattern: wire payloads are plain data with
`pub` fields; invariants live in validated newtypes (`SessionId`). Check: dylint `wirepub` (a payload type — one with a
serde impl — with a private field, in the `wire` crate).

## PRIVACY
Anti-pattern: a name, host, path or address from the lab reaching the public repository. Pattern: fixtures use /home/user,
example.com, 00000000-0000-4000-8000-… ids, AcmeKit; denylisted values are matched by hash, so the plaintext is never
present to leak. Check: xtask `PRIVACY` (hashed privacy denylist over tracked files and staged lines).

## SECRET
Anti-pattern: a live credential in the repository (API keys, tokens, private keys, relay keys). Pattern: keep credentials
out; the scanner matches exact known-sensitive strings by hash and generic token shapes. Check: xtask `SECRET` (hashed
secret denylist plus token-shape rules over tracked files and staged lines).

## AUTHORSHIP
Anti-pattern: commits by unrecognised identities or carrying attribution trailers. Pattern: one Conventional Commit under
a house identity per lane; no trailers. Check: xtask `AUTHORSHIP` (identity against `xtask/data/house-identities.json`
hashes; no Co-authored-by / Signed-off-by / AI-attribution text), enforced by the commit-msg hook and the CI range check.

## MANIFEST
Anti-pattern: a crate manifest drifting from the workspace (its own version, a pinned dependency, no `[lints] workspace`), a workspace member depending on the `rivethub` binary, a runtime crate reaching the `migrate` crate through a normal dependency (schema changes are applied only by `rivethub-migrate`, outside the runtime; the xtask check walks the workspace's normal-dependency graph transitively, exempting only the direct `cli` → `migrate` edge for schema verification; dev-dependencies stay legal), or the `migrate` crate depending on anything but `wire`/`config` and external crates through a normal dependency. Pattern: every member inherits `package.*` from `[workspace.package]`, takes dependencies from `[workspace.dependencies]`, carries `[lints] workspace = true`, and never names the composition-root binary as a dependency. Check: xtask `MANIFEST` (via `cargo_metadata` and `toml`).

## TOOLCONFIG
Anti-pattern: a tool configuration file below the workspace root — a crate's own `clippy.toml` (or
`.clippy.toml`, `rustfmt.toml`, `.rustfmt.toml`, `deny.toml`, `.cargo/config.toml`, `rust-toolchain`,
`rust-toolchain.toml`): Clippy reads the nearest configuration file, so a crate-level file silently
replaces the workspace's disallowed types and methods for that crate, and a member manifest without
`[lints] workspace = true` or with its own `[lints.*]` tables walks out of the workspace policy the
same way. Pattern: tool configuration lives only at the workspace root (a directory listed in
`[workspace] exclude` is its own workspace root); every member manifest inherits
`[lints] workspace = true`, and a spot where a workspace lint is genuinely wrong carries
`#[expect(lint, reason = "…")]`. Check: xtask `TOOLCONFIG`.

## SIZE
Anti-pattern: a production file growing past the point where a reviewer can hold it. Pattern: split by responsibility; a
file far over the ceiling means the lane is porting, not designing. Check: xtask `SIZE` (600 non-blank lines per
production file under `crates/`).

## TESTPROTO
Anti-pattern: a test target whose process does not speak the libtest list protocol — a `harness = false` helper with a hand-rolled `main` runs under `cargo test` but dies on `--list`, so CI's nextest list fails and only CI finds out — and, the same mistake one level up, a helper that exists to be spawned as a process living as a test target at all. Pattern: every test target speaks the libtest protocol (a custom `main` is a libtest-mimic harness); helpers that are processes are bins the tests name with `CARGO_BIN_EXE_*`, never a scan of `target/`. Check: xtask `TESTPROTO` (`cargo nextest list --workspace --all-features`, the same list CI runs; a target that rejects `--list` fails the local gate, and a missing cargo-nextest is a typed missing-tool error naming the install command).

## PATHCONTAIN
Anti-pattern: path containment by string — `str::starts_with` on the result of `to_string_lossy`, `to_str` or
`display().to_string()`. String prefixes admit `..` siblings, case and separator surprises. Pattern: containment uses
`Path::starts_with` on normalized paths (`config`'s `paths::normalize`); string prefix tests stay for plain labels that
never come from a path. Check: xtask `PATHCONTAIN` (a `starts_with` call whose receiver chain ends in a path conversion).

## WAIVER
Anti-pattern: an inner attribute waiver — `#![expect(...)]` or `#![allow(...)]` under `crates/`, tests included. A
file-wide expectation hides which spot actually needs it and keeps waiving after the code moves on. Pattern: the
expectation sits on the one item that needs it (a shared test harness included whole by several binaries carries its
expectations on the `mod common;` declaration of each includer, where the per-binary subset is known).
Check: xtask `WAIVER` (every inner attribute naming `expect` or `allow`).

## REASON
Anti-pattern: an imitation reason — an `#[expect]` whose `reason` names mirroring the recording, TypeScript or JavaScript
(`mirror`, `recorded`, `TypeScript`, `JavaScript`, `JS`, case-insensitive). Imitation is not a reason: it justifies code by
what it copies instead of the rule it follows. Pattern: the reason states the rule — the contract row, the documented
default, the invariant that makes the lint wrong at that spot. Check: xtask `REASON` (the `reason` string of every
`#[expect]`, `#[cfg_attr(dylint_lib = "rr_lints", expect(...))]` included).

## UTF16
Anti-pattern: UTF-16 emulation — `char::len_utf16`, `str::encode_utf16`, identifiers containing `utf16` or ending
`_units`, outside `wire` and the one capture event-id hash (whose `#[expect]` reason names that rule). Pattern: lengths and
caps are chars or bytes; the contract defines what is observable (CONTRACT §2), and UTF-16 arithmetic reproduces the
JavaScript runtime, not the boundary. Check: xtask `UTF16`; existing hits live in `xtask/baseline.toml` (see BASELINE).

## JSONBYTES
Anti-pattern: a hand-rolled JSON byte scanner — matching on `b'"'` together with `b'{'`/`b'}'`. JSON goes through serde;
string awareness, escapes and nesting are the format crate's job. Pattern: serde types; the contract's one byte-level
exception (the lone-surrogate escape handling at capture ingest) is the named function
`wire::json::replace_lone_surrogates`, and the check exempts `crates/wire/` by path.
Check: xtask `JSONBYTES`; existing hits live in `xtask/baseline.toml` (see BASELINE).

## PORTS
Anti-pattern: a service or adapter declares a public trait for a store or external boundary. Pattern: ports live in
kernel, adapters implement them, and the composition root injects them. Internal seams use restricted visibility;
a sealed extension trait needs a trait-local `#[expect(..., reason = "PORTS: ...")]` explaining why it is not a port.
Check: xtask `PORTS` parses Rust with syn and flags `pub trait` declarations in gateway, workers, mcp, capture,
providers, harness-acp, memory-postgres and memory-sqlite, including nested modules.
Restricted traits and traits with a direct outer expectation whose reason cites the exact `PORTS` id are exempt;
module-level and conditional expectations do not waive declarations. Existing ports are counted in
`xtask/baseline.toml` by `cargo xtask baseline --write`; new occurrences fail and stale entries must be removed.
Services never touch databases directly: cargo-deny restricts `sqlx` and `rusqlite` to memory-postgres, memory-sqlite,
migrate, dev-only testkit, and composition crates rivethub and cli through `deny.toml` wrappers.
`capture` is also a `rusqlite` wrapper for read-only access to external harness stores.
The third-party SQLx vector codec pgvector is an additional sqlx wrapper, restricted in turn to memory-postgres;
services cannot bypass the database ban through it.
The PORTS metadata check rejects normal and build dependencies on testkit, including renamed and target-specific edges.

## KERNELWIRE
Anti-pattern: a kernel type, port or rule that imports `wire`: a contract DTO in a port signature, a domain enum defined
in `wire::gateway` and re-exported by the kernel, a kernel function that builds a response body. The published contract
then dictates the domain, and a contract change ripples through every adapter. Pattern: the kernel owns its domain types
(topic kinds, Tidy proposals, plug-in manifests, wiki pages and patches, memory records, transcript turns, harness
offerings); the gateway's routes and the adapters translate between them and `wire` at the edge. The value types both
sides share unchanged (session and harness ids, the free-form JSON holders, the `Millis` time format, a conversation's
messages, plug-in names) live in the `values` crate, which depends on neither, and `wire` re-exports them. Check: the
cargo-deny ban on `wire` in `deny.toml` leaves `kernel` out of its wrappers, so a `wire` dependency in the kernel's
manifest fails the gate and a `wire::…` path in the kernel does not compile.

## BASELINE
Anti-pattern: a scan whose existing hits are waived wholesale (a free allow-list) or whose history is rewritten (line
numbers that churn, entries re-added after a fix). Pattern: `xtask/baseline.toml` holds one entry per existing hit — check
id, file, matched text and occurrence count, no line numbers. `cargo xtask baseline --write` regenerates the counts
from the syn scans and typed Dylint diagnostics; a second occurrence consumes a second count. The check fails on a hit with no entry and on an entry that no longer matches
code, so a fixed hit is removed in the same commit; an added entry or increased count must already exist at the merge-base with `origin/main`
(the baseline can only shrink). The commit that introduces the file sets the floor: the shrink leg only ever reports when
it actually compared — if `origin/main` is absent, the merge-base cannot be resolved (a shallow or partial fetch), the
merge-base carries no `xtask/baseline.toml`, the leg passes without a finding. A malformed comparable baseline fails the gate.
From the first comparable commit on, every entry must trace back to the merge-base. A check the merge-base's
`xtask/src/baseline.rs` does not name yet sets its own floor the same way, in the commit that introduces it. Check: xtask `BASELINE`.

## PATTERNS
Anti-pattern: a rule with no written entry, or an entry whose enforcement drifted. Pattern: every lint and check name has
an entry here and every entry names the lint or check that enforces it. Check: xtask `PATTERNS`.
