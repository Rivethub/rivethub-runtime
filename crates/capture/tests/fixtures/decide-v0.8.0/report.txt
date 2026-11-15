# Sanitize report for bundle.json

The sanitizer is best-effort. It reliably catches known vendor tokens, assignments to password- and token-named keys in env, JSON, YAML, XML, SQL and code, name/value pairs with such names, URL credentials, PEM and DER private keys, base64 blobs that decode to any of those, and mapped terms. It can miss unusual code forms, vendor shapes it has no rule for, and passwords written as bare words in prose or with spaces outside quotes. Read this report before the bundle leaves the machine.

Sanitized 1 session at 2026-11-08 12:00:00 UTC: 0 secrets, 0 mapped, 0 redacted.

Never share this report or send it with the bundle. Its Flags section shows the text of each flagged chunk, including the text the bundle withholds, so it is for you alone. Never share `<out without .json>.review.local.json` either, which holds the withheld text; share only the bundle.

The tables below never show the text the secrets and mapping passes replaced, but the Flags section does show flagged text. The Original column is a short HMAC of each replaced span under this machine's local sanitize key and the raw bundle's digest, so two spans with the same text share a fingerprint and nobody without the key can test a guess against it.

Every text field is scanned for secrets, the author included; the author and task id are not mapped. Dates and roles are not scanned. Event ids are replaced by an HMAC of the raw id under the local key.

- Secret rules: 68 RivetHub rules and 215 gitleaks rules (v8.30.1)
- Mapping: none
- Policy sha256: `228a773040001a72ff039fc2139ab1c978645e6834dd7720d8ca4c8949af6e07`
- Bundle sha256: `738dfb1c224d2f80d252a1dc947a6faee3003124ef5f50f22cb93e8fe148fb20`
- Stamp: `2026-11-08T12:00:00.123456789Z`

## Flags to review

After the secrets and mapping passes, the decision model `stub-model` scored 10 chunks in 6 calls. It skipped 1 trivial chunk: identifiers (UUIDs, hex digests, base64url ids, times, and metadata id and path fields), placeholders, `[tool call] <name>` lines, and bare tokens under 16 characters that hold none of `@ / = , : ;`, no capitalized word, no non-ASCII letter, no mix of letters and digits, and no IP, phone or host-name shape, such as numbers and enum values. Below are the chunks it scored highest, up to 20 per session, at or over 0.30. Each flag covers every place in the bundle, in any session and inside other flagged text, JSON keys, tool names, channels and task ids, where its text occurs outside a placeholder.

This review blocks. Until you mark a flag, bundle.json holds `[FLAGGED:<n>]` in place of its text at every exact occurrence the review found. The same text in another form, such as JSON-escaped in a tool result or inside a larger unflagged chunk, is not guaranteed to be withheld or marked. Ingest refuses the bundle while any flag is open.

Unreviewed: 3 of 3

To review a flag, replace `open` on its Mark line with one of these marks, then run `rh memory sanitize --resolve <this report>`:

- `keep` puts the text back as shown.
- `redact` replaces the text with `[REDACTED:flagged]`.
- `map <term> as <placeholder>` replaces each `<term>` in the text with `<placeholder>`; a term that starts or ends with a letter, digit or underscore matches whole words only. The placeholder may not be a `[FLAGGED:<n>]` or `[REDACTED...]` form or look like a secret. Add `--learn` to also add the rule to your mapping file (terms of 3 or more characters).

Do not edit the text under a flag: `--resolve` refuses a flag whose text changed. You can mark some flags now and the rest later; a flag whose section you delete stays open.

### Flag 1 (score 0.91)

- Session: `claude-code:c1`
- Id: `claude-code:c1:task_id`
- Also at: `2b2299ca35ea65b4b561b9af2c7134363a767d14cf0eee2086d26a24b4fc3f39:content`

Mark: open

~~~text
Ran the tests.
Acme Corp is churning next quarter.
~~~

<!-- flag 1: UmFuIHRoZSB0ZXN0cy4KQWNtZSBDb3JwIGlzIGNodXJuaW5nIG5leHQgcXVhcnRlci4= -->

### Flag 2 (score 0.91)

- Session: `claude-code:c1`
- Id: `b754fec4cd9715413fe5e3a2307bf8238376d2d2189cad6f9c89043e665a061e:tool_args.command`
- Also at: `c1997b50d46ae38f027711307c99890a4d10850b87ffdbff2718542a68fe1af4:tool_args.command`

Mark: open

~~~text
grep churn Acme Corp.csv
~~~

<!-- flag 2: Z3JlcCBjaHVybiBBY21lIENvcnAuY3N2 -->

### Flag 3 (score 0.55)

- Session: `claude-code:c1`
- Id: `b754fec4cd9715413fe5e3a2307bf8238376d2d2189cad6f9c89043e665a061e:tool_result`

Mark: open

~~~text
churn rows: 3
~~~

<!-- flag 3: Y2h1cm4gcm93czogMw== -->

## Changes per rule

| Pass | Rule | Replaced with | Changes |
|---|---|---|---|
| - | no rule matched | - | 0 |

## Session 1: c1 (rivet, 4 messages)

No changes.

