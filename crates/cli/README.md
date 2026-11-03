# RivetHub certificate commands

The PKI CLI implements the approved design in `docs/PKI.md` and the step-ca addendum. `rh ca` mints one-time tokens and revokes serials by running the `step` CLI against a step-ca the operator already bootstrapped. Node and agent names come from the operator roster, never from `/mesh.json`. `rh cert renew` talks to step-ca. Its acceptance tests replay `crates/testkit/tests/fixtures/stepca/api.json` through a local HTTPS signer. No production CA material is read or checked in.

## Operator commands

`--ca-url` and `--root` are required. `--provisioner-password-file` is required for `token` and `revoke`. `--roster` is required for node and agent tokens. A roster whose file name is `mesh.json` is refused. `rh ca` does not create a CA and does not read CA state.

- `rh ca token node <name> --ca-url <url> --root <pem> --provisioner-password-file <file> --roster <file>` runs `step ca token`. Each roster line is `node:ip:agent`. The token subject is the node name, and the pinned names are that node's IP, `<name>`, and `<name>.rivet.mesh`.
- `rh ca token agent <name>` uses the same flags. `<name>` must be the agent field of one roster line. The subject and only SAN are `spiffe://rivet.mesh/agent/<node>/<agent>`.
- `rh ca token device <name>` does not read the roster. The subject and only SAN are `<name>`, one unreserved label.
- `rh ca revoke <serial>` runs `step ca revoke` for one canonical positive decimal serial.
- `rh ca crl` prints the PEM CRL from `stepca::Authority::crl`. `--crl-out <path>` publishes that PEM atomically, mode 0644, and is ignored on the other commands.
- `rh ca status` prints `Certificate authority ready.` and each root subject and expiry from `stepca::Authority::roots`.

## Holder commands

```text
rh cert renew --principal spiffe://rivet.mesh/node/node-a \
  --ca-url https://ca.example:9000 \
  --trust-root /trusted/root.cert.pem \
  --token-file /protected/enrollment-token
```

The first run, and any later run that passes `--token-file`, generates a P-256 private key on the holder when `tls/holder.key.pem` is absent. The key never leaves the holder. The command reads the `sans` claim from the step-ca one-time token, builds a CSR containing exactly those names, and calls `stepca::Authority::sign`. step-ca verifies the token. The holder does not put the principal into the CSR. The issued certificate must contain that SPIFFE URI and every name the token pinned, and no other name. Renewal with no token calls `stepca::Authority::renew` over mTLS using the current certificate and key.

The supplied root is the only HTTPS trust anchor and the only chain anchor. The CA URL must be an HTTPS origin; credentials, query strings and fragments are refused. Same-origin 307 and 308 redirects are followed. A redirect to another origin is refused before the request is sent there.

Holder state defaults to `~/.rivethub`, and `--state` selects another directory. The files live in `{state}/tls/`: `holder.key.pem`, `holder.cert.pem` (leaf, then intermediate), and `renew.lock`. The directory is mode 0700. The key and certificate are mode 0600 and have one hard link. Publication writes a temporary file in that directory, sets mode 0600, and renames it into place. Before replacement, the certificate is checked against the pinned root, the local public key, client-auth use, the validity window, the principal URI, and the requested names. A certificate that has already reached two thirds of its lifetime is refused. Renewal keeps the existing key. A failed request leaves the current files in place. Symlinks, `..`, a shared-prefix sibling, and a linked or permissive key are refused. Paths with spaces and relative paths are accepted when they stay inside the state directory.

Without `--once`, the process waits until two thirds of the certificate's actual lifetime, then renews. `--once` enrolls or renews once and exits. Cancellation before the command starts is a failure. Cancellation during the wait or an in-flight request returns success and does not publish a certificate. An atomic publish that has already started is finished. The command does not re-enroll silently when renewal fails. A later process that passes `--token-file` enrolls again; a spent token is a CA refusal and the stored files stay. Successful holder commands print no stdout and do not log the token.

## Coverage and exclusions

`ca` tests mint tokens through a fake `step` on `PATH`, refuse roster and path escapes, and replay the recorded roots and CRL. `cert` tests use a local TLS server that signs the submitted CSR and returns the recorded `/1.0/sign` and `/1.0/renew` document shape, including the recorded refusal text. They cover token-SAN enrollment, mTLS renewal, key locality, same-origin and foreign redirects, a wrong key, a wrong principal, an extra name, a reused token, a revoked renewal, cancellation, and unsafe paths. Paused-clock tests cover the renewal timer. `stepca_scope` requires every case in the step-ca recording, plus the recorded CRL, to be exercised or skipped with a reason. This command skips `sign-san-not-granted`, `renew-without-certificate`, `revoke-own-mtls`, `roots`, and `crl`: it never builds an ungranted CSR, never renews without a certificate, and it does not revoke, fetch roots, or read the CRL. `pki_scope` still requires an empty `tests/fixtures/pki/skipped.json`.

`rh ca token` and `rh ca revoke` call the shared minter in `pki`. The gateway's production certificate authority is that same adapter when `gateway.certificate_authority` is set. The test server is an acceptance fixture.

# RivetHub systemd commands

`rh service init|start|stop|restart|status|logs` implements B9's service contract in
`crates/testkit/tests/fixtures/cli/README.md`. Effective UID selects system mode
(`/etc/systemd/system/rivethub.service`) or user mode
(`~/.config/systemd/user/rivethub.service`). Init generates the unit from `service::Unit`
using this executable's absolute path and the configured state directory's `.env`.
It publishes the file exclusively, reports user lingering, and refuses existing
files, traversal and directory links escaping the home directory. Systemd quoting
preserves spaces, quotes, Unicode, dollar signs and percent signs literally.

Start and restart reload the manager, execute the action and show status. A failed
reload or action stops the sequence and returns failure with stderr. Status retains
the child exit code. Logs inherits the terminal streams and follows the journal until
interrupted; cancellation kills and reaps the child before returning success. All
commands use `tokio::process`; blocking account lookup and unit publication run on
`spawn_blocking`.

The fixture directory currently contains the written contract only, with no captured
CLI transcripts. The `service` acceptance tests exercise every service row using
fake `systemctl`, `journalctl` and `loginctl` executables in a temporary PATH. They
check both scopes, unit serialization, overwrite refusal, path containment, quoting,
status codes, subprocess errors and event-driven cancellation. No service scenario is
skipped; the existing checked deferred list covers the remaining CLI commands in
that same fixture README. `rh start` supervises `rh gateway`, `rh worker --role`
for the roles the configuration switches on, and `rh mcp` when its network
transport is configured. It holds the state-directory pid lock, restarts children
with backoff, and forwards SIGTERM. `Config::load` with `Profile::Node` and
`rh config validate` still require `runtime`, `agents`, and `providers`. `rh local`
does not invent those values. With no resolved file it writes the SQLite path and
the gateway bind, loads that file through `Config::load` with `Profile::Standalone`,
migrates it, and claims the state directory. The task engine runs in that process
unless `tasks.enabled` is false.

# RivetHub mesh commands

`rh mesh` lists, probes, enrolls, renews, syncs, joins, and forgets mesh nodes. The registry is the gateway mesh file. Enrollment and renewal use step-ca: enroll mints a one-time token from the operator roster and posts `/1.0/sign`; renew uses holder mTLS. The hub address on enroll, renew, and sync is checked and is not an SSH target. `rivethub-hub` and `rivet-ca.sh` are retired. `join` without `--manual` points at enroll. `join --manual` fetches `/mesh.json` and posts `/api/mesh/join`.
