# pki

`pki` is the certificate-authority adapter. It implements `kernel::ca::CertificateAuthority` on top of the `stepca` HTTP client and owns the one `step ca token` / `step ca revoke` minter used by `rh ca` and by the gateway.

`Authority::connect` reads the configured root PEM, checks the provisioner password file and the optional roster path, and opens a `stepca::Authority`. Sign with a token calls `POST /1.0/sign`. Renewal and `issue_token` mint a token for the principal, then sign. `chain` is the intermediate from the last successful sign followed by `GET /roots`. `crl` is `GET /1.0/crl?pem=true`. `revoke` runs `step ca revoke` for each decimal serial this process issued to that SPIFFE principal.

Node and agent token SANs come from the operator roster (`node:ipv4:agent`). Device principals use `spiffe://rivet.mesh/device/<user>/<device>` as the subject and only SAN, for device pairing and for `rh ca token device <device> --user <user>` alike. There is no local signer, sqlite store, CRL builder, or CA import. step-ca is bootstrapped outside this runtime. A transport failure, cancellation, or a missing `step` binary is `CaError::Unavailable`.

The port lives here, not in `stepca`, because `stepca` is not allowed to depend on `kernel`.

Tests replay `crates/testkit/tests/fixtures/stepca/api.json`. Refusal text is the recorded message. Success sign and roots use a local TLS stub with the step-ca JSON shape, because the recording does not include the root private key. The recorded CRL PEM is returned verbatim. Holder mTLS cases and the retired local SAN-policy scenarios are named in `tests/fixtures/skipped.json`.
