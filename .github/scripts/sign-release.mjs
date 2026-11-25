#!/usr/bin/env node
import { createPrivateKey, createPublicKey, sign, verify } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const PKCS8_ED25519_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const SPKI_ED25519_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const PIN_DIRECTORY = fileURLToPath(new URL("../../crates/gateway/release-keys/", import.meta.url));
const SWITCH = "REQUIRE_SIGNED_RELEASES in crates/gateway/src/account/update.rs";

function loadSigningKey(raw) {
  const trimmed = raw.trim();
  if (trimmed.includes("BEGIN")) {
    return createPrivateKey(trimmed);
  }
  const seed = /^[0-9a-fA-F]{64}$/.test(trimmed)
    ? Buffer.from(trimmed, "hex")
    : Buffer.from(trimmed, "base64");
  if (seed.length !== 32) {
    throw new Error(`RELEASE_SIGNING_KEY must be PKCS8 PEM or a 32-byte Ed25519 seed (got ${seed.length} bytes).`);
  }
  return createPrivateKey({ key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]), format: "der", type: "pkcs8" });
}

function pinnedKey(keyId) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(keyId)) {
    throw new Error(`RELEASE_SIGNING_KEY_ID '${keyId}' is not a valid key id.`);
  }
  const file = `${PIN_DIRECTORY}${keyId}.hex`;
  if (!existsSync(file)) {
    throw new Error(`No client pins key id '${keyId}' (expected crates/gateway/release-keys/${keyId}.hex).`);
  }
  const hex = readFileSync(file, "utf8").trim();
  if (!/^[0-9a-f]{64}$/.test(hex)) {
    throw new Error(`crates/gateway/release-keys/${keyId}.hex is not 64 lowercase hex digits.`);
  }
  return createPublicKey({ key: Buffer.concat([SPKI_ED25519_PREFIX, Buffer.from(hex, "hex")]), format: "der", type: "spki" });
}

function spki(key) {
  return key.export({ format: "der", type: "spki" });
}

const files = process.argv.slice(2);
if (files.length === 0) {
  console.error("Usage: sign-release.mjs <file> [<file> ...]");
  process.exit(2);
}

const rawKey = process.env["RELEASE_SIGNING_KEY"] ?? "";
if (!rawKey.trim()) {
  process.stderr.write(
    `::warning::RELEASE_SIGNING_KEY is not set; publishing without .sig files. Clients accept this unsigned release only while ${SWITCH} is false.\n`,
  );
  process.exit(0);
}

const keyId = (process.env["RELEASE_SIGNING_KEY_ID"] ?? "").trim() || "2026-main";
const privateKey = loadSigningKey(rawKey);
if (privateKey.asymmetricKeyType !== "ed25519") {
  throw new Error(`RELEASE_SIGNING_KEY must be an Ed25519 key (got ${privateKey.asymmetricKeyType}).`);
}
const publicKey = pinnedKey(keyId);
if (!spki(createPublicKey(privateKey)).equals(spki(publicKey))) {
  throw new Error(`RELEASE_SIGNING_KEY is not the private half of pinned key '${keyId}'.`);
}

for (const file of files) {
  const payload = readFileSync(file);
  const sig = sign(null, payload, privateKey);
  if (!verify(null, payload, publicKey, sig)) {
    throw new Error(`Verification against pinned key '${keyId}' failed after signing ${file}`);
  }
  writeFileSync(`${file}.sig`, `v1:${keyId}:${sig.toString("base64")}\n`);
  console.log(`Signed ${file}.sig (key ${keyId})`);
}
