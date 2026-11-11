# Sanitize corpora

Each directory is one corpus of fake secrets and real-looking text that `rh memory sanitize` must handle. `tests/it/sanitize_corpus.rs` runs every `cases.jsonl` line through `capture::sanitize::run` with a fixed local key and no mapping, and one test per directory fails with the list of shapes that leaked or changed. A second test runs every case of every corpus through `capture::sanitize::secrets`, the scan `rh memory ingest-bundle` runs on a sanitized bundle, and fails when a sanitized case still matches or needed sanitize's own re-check, because either one is a sanitizer bug. A third test sends every case through the decision review with a stub model that flags every chunk, advisory and `--block`, before and after `--resolve` with mixed marks, and fails on any bundle ingest would refuse.

## Case lines

One JSON object per line:

| Field | Meaning |
|---|---|
| `shape` | A name unique within the corpus, in `snake_case`. |
| `position` | Where the text sits in the bundle: `content`, `tool_result`, `tool_args_value`, `tool_args_key`, `tool_args_json`, `metadata_value`, `metadata_key`, `title`, `settings_value`, `session_key`, `channel`, `task_id`, `tool_name`, `agent`, `origin_node`, `event_id`, or `*` for every one of them. `tool_args_json` takes `raw` as the whole JSON object. |
| `raw` | The text placed at that position. |
| `planted` | The fake secret values inside `raw`. Required when `expect` is `redact`. |
| `expect` | `redact`: none of `planted` may remain in the sanitized bundle or its report. `keep`: the field must come back byte-identical (a false positive otherwise). |
| `note` | Optional; the fix-list item, verifier lane or reason, appended to a failure line. |

Every value is fake. A planted value carries `PLANTED` where the format allows; otherwise it is seeded random text in the vendor's alphabet inside a carrier that says `PLANTED`. Never paste a real credential into a corpus, even a revoked one. One character of every token-like run is written as a JSON `\uXXXX` escape (`sk_liv\u0065_...`), so GitHub's push protection does not mistake a fake for a real credential; the parsed value is unchanged, and a new line that is not escaped is rejected at push time with the line to fix.

## Corpora

- `vendor-shapes-r1`: the round-1 vendor token, URL, PEM, env and config shapes (65).
- `unseen-shapes-r2`: the round-2 vendor and cloud shapes the first ruleset had never seen (73).
- `vendor-and-code-shapes-r3`: round-3 vendor CLIs, cookies, SQL, PHP, Ruby, Go, Java and Rust forms, plus two controls that must stay (82).
- `placement-and-forms`: a planted value in every bundle field, assignment separators, base64 blobs, placeholder boundaries, name-table forms, and SS-4's `[REDACTED:flagged]` and flag look-alikes (104).
- `real-output-contexts-r3`: snippets of real tool output (CI workflows, OAuth library source, lockfiles, OpenAPI specs, Rust code) that a sanitizer must leave alone, plus the planted secrets inside that corpus (232).
- `round-3-fixes`: the cases behind fix-list items 2, 4, 5, 7, 8 and 9-12 (47).
- `round-4-fixes`: shell parameter expansions kept whole, URL users, bracket tails, long YAML block scalars, a PEM block followed by code, pretty-printed JSON under secret names, name/value pairs (Kubernetes env, ECS, HAR), `gh secret set`, and prose after a secret word (43).

The corpora hold 646 cases.

## Adding a shape

1. Reproduce the leak or false positive as one `cases.jsonl` line in the corpus it belongs to (a new vendor shape goes in the newest `vendor-*` corpus; real output that must stay goes in `real-output-contexts-*`). Run `cargo nextest run -p capture sanitize_corpus` and watch it fail on that line.
2. Add the rule as data in `crates/capture/src/rules/rivethub.toml`: a `[[rules]]` table with a regex whose first group (or `secretGroup`) is the secret, its `keywords` for the prefilter, and optional `entropy`, `each` or `decode`; or a name in the `[names]` table (`password`, `token`, `key_never`, `exact_*`, `stopwords`, `templates`, `code_values`, `comment_openers`, `separators`, `vendor_prefixes`). Code changes only when no data form expresses the shape.
3. Run the corpus tests again. A new rule that makes a `keep` case fail is an over-redaction; narrow it or record the trade in the PR.
4. A new corpus directory needs a `corpus_tests!` line in `sanitize_corpus.rs`; `every_corpus_directory_has_a_test` fails until it has one.

The sanitizer is best-effort. These corpora pin what it catches today; CONTRACT note 136 states what it catches by design and what it may miss.
