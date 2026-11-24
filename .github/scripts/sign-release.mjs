#!/usr/bin/env node
import { createPrivateKey, createPublicKey, sign, verify } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

const PKCS8_ED25519_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

function loadSigningKey(raw) {
  const trimmed = raw.trim();
  if (trimmed.includes("BEGIN PRIVATE KEY")) {
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

const files = process.argv.slice(2);
if (files.length === 0) {
  console.error("Usage: sign-release.mjs <file> [<file> ...]");
  process.exit(2);
}

const rawKey = process.env["RELEASE_SIGNING_KEY"] ?? "";
if (!rawKey.trim()) {
  process.stderr.write("::warning::RELEASE_SIGNING_KEY is not set in release-signing environment; publishing without Ed25519 .sig\n");
  process.exit(0);
}

const keyId = (process.env["RELEASE_SIGNING_KEY_ID"] ?? "2026-main").trim() || "2026-main";
const privateKey = loadSigningKey(rawKey);
const publicKey = createPublicKey(privateKey);

for (const file of files) {
  const payload = readFileSync(file);
  const sig = sign(null, payload, privateKey);
  if (!verify(null, payload, publicKey, sig)) {
    throw new Error(`Self-verification failed after signing ${file}`);
  }
  writeFileSync(`${file}.sig`, `v1:${keyId}:${sig.toString("base64")}\n`);
  console.log(`Signed ${file}.sig (key ${keyId})`);
}
