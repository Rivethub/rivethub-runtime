import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { generateKeyPairSync, verify } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const SIGNER = fileURLToPath(new URL("./sign-release.mjs", import.meta.url));

function pem(privateKey) {
  return privateKey.export({ format: "pem", type: "pkcs8" }).toString();
}

function rawPublicKey(publicKey) {
  return publicKey.export({ format: "der", type: "spki" }).subarray(-32);
}

function signerPinning(id, publicKey) {
  const root = mkdtempSync(join(tmpdir(), "sign-release-"));
  mkdirSync(join(root, ".github/scripts"), { recursive: true });
  mkdirSync(join(root, "crates/gateway/release-keys"), { recursive: true });
  copyFileSync(SIGNER, join(root, ".github/scripts/sign-release.mjs"));
  writeFileSync(join(root, `crates/gateway/release-keys/${id}.hex`), `${rawPublicKey(publicKey).toString("hex")}\n`);
  return join(root, ".github/scripts/sign-release.mjs");
}

function run(script, env) {
  const directory = mkdtempSync(join(tmpdir(), "sign-release-payload-"));
  const file = join(directory, "SHA256SUMS");
  writeFileSync(file, "00  rh-0.8.19-x86_64-unknown-linux-gnu.tar.gz\n");
  const { RELEASE_SIGNING_KEY: _key, RELEASE_SIGNING_KEY_ID: _id, ...inherited } = process.env;
  const result = spawnSync(process.execPath, [script, file], { env: { ...inherited, ...env }, encoding: "utf8" });
  return { ...result, file };
}

test("signs with the pinned key, and the signature verifies against the pin", () => {
  const pair = generateKeyPairSync("ed25519");
  const result = run(signerPinning("test-key", pair.publicKey), {
    RELEASE_SIGNING_KEY: pem(pair.privateKey),
    RELEASE_SIGNING_KEY_ID: "test-key",
  });
  assert.equal(result.status, 0, result.stderr);
  const [version, id, signature] = readFileSync(`${result.file}.sig`, "utf8").trim().split(":");
  assert.equal(version, "v1");
  assert.equal(id, "test-key");
  assert.ok(verify(null, readFileSync(result.file), pair.publicKey, Buffer.from(signature, "base64")));
});

test("accepts a 32-byte hex seed for the pinned key", () => {
  const pair = generateKeyPairSync("ed25519");
  const seed = pair.privateKey.export({ format: "der", type: "pkcs8" }).subarray(-32).toString("hex");
  const result = run(signerPinning("test-key", pair.publicKey), {
    RELEASE_SIGNING_KEY: seed,
    RELEASE_SIGNING_KEY_ID: "test-key",
  });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(existsSync(`${result.file}.sig`));
});

test("refuses an Ed25519 key that is not the client's pinned 2026-main key", () => {
  const pair = generateKeyPairSync("ed25519");
  const result = run(SIGNER, { RELEASE_SIGNING_KEY: pem(pair.privateKey) });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /not the private half of pinned key '2026-main'/);
  assert.ok(!existsSync(`${result.file}.sig`));
});

test("refuses an RSA key", () => {
  const pair = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const result = run(SIGNER, { RELEASE_SIGNING_KEY: pem(pair.privateKey) });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /must be an Ed25519 key \(got rsa\)/);
  assert.ok(!existsSync(`${result.file}.sig`));
});

test("refuses a key id no client pins", () => {
  const pair = generateKeyPairSync("ed25519");
  const result = run(SIGNER, { RELEASE_SIGNING_KEY: pem(pair.privateKey), RELEASE_SIGNING_KEY_ID: "unpinned" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /No client pins key id 'unpinned'/);
  assert.ok(!existsSync(`${result.file}.sig`));
});

test("publishes unsigned with a warning that names the enforcement switch", () => {
  const result = run(SIGNER, {});
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /^::warning::.*REQUIRE_SIGNED_RELEASES in crates\/gateway\/src\/account\/update\.rs/m);
  assert.ok(!existsSync(`${result.file}.sig`));
});
