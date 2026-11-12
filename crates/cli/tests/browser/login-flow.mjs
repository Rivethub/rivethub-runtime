import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

const RH = process.env.RH_BIN;
const PLAYWRIGHT_DIR = process.env.PLAYWRIGHT_DIR;
const PORT = Number(process.env.RH_PORT ?? 62540);
const WANT = (process.env.BROWSERS ?? "chromium").split(",");
const EXECUTABLES = { chromium: process.env.CHROMIUM_PATH };
if (!RH || !PLAYWRIGHT_DIR) {
  console.error("set RH_BIN (path to rh) and PLAYWRIGHT_DIR (dir with node_modules/playwright)");
  process.exit(2);
}
const playwright = createRequire(join(PLAYWRIGHT_DIR, "x.js"))("playwright");

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

async function waitHealthy(origin) {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${origin}/healthz`)).status === 200) return;
    } catch {}
    await sleep(200);
  }
  throw new Error("gateway never became healthy");
}

function startOtherOrigin(port) {
  const server = createServer((_, res) => {
    res.setHeader("content-type", "text/html");
    res.end("<!doctype html><title>other origin</title>");
  });
  return new Promise((done) => server.listen(port, "127.0.0.1", () => done(server)));
}

async function runBrowser(name, home, origin, otherOrigin) {
  const results = [];
  const check = (label, ok, detail = "") => {
    results.push({ label, ok });
    console.log(`  ${ok ? "PASS" : "FAIL"} ${label}${detail ? ` (${detail})` : ""}`);
    assert.ok(ok, label);
  };
  const browser = await playwright[name].launch({ executablePath: EXECUTABLES[name] || undefined });
  try {
    const context = await browser.newContext();
    const page = await context.newPage();

    const opened = spawnSync(RH, ["open", "--no-browser"], { env: { HOME: home }, encoding: "utf8" });
    assert.equal(opened.status, 0, opened.stderr);
    const url = new URL(opened.stdout.trim());
    check("rh open prints a fragment, not a query", url.hash.startsWith("#code=") && url.search === "");

    await page.goto(url.href);
    await page.waitForURL((u) => u.pathname === "/" && u.hash === "", { timeout: 15000 });
    const final = new URL(page.url());
    check("fragment is stripped after login", final.hash === "" && !page.url().includes("code="), page.url());

    const cookies = await context.cookies(origin);
    const session = cookies.find((c) => c.name.endsWith("rh_session"));
    check("session cookie is set", Boolean(session), session?.name);
    check("cookie is HttpOnly", session?.httpOnly === true);
    check("cookie is SameSite=Strict", session?.sameSite === "Strict", session?.sameSite);
    check("cookie is not Secure on http loopback", session?.secure === false, String(session?.secure));
    check("cookie is not readable from script", !(await page.evaluate(() => document.cookie)).includes("rh_session"));

    const csrf = await page.evaluate(() => localStorage.getItem("rh-csrf"));
    check("login page stored a CSRF token", Boolean(csrf));
    const call = (headers, path = "/api/memory/search?q=x", init = {}) =>
      page.evaluate(
        async ([path, headers, init]) => (await fetch(path, { credentials: "same-origin", headers, ...init })).status,
        [path, headers, init],
      );
    const withHeader = await call({ "x-rh-csrf": csrf });
    check("API call with the CSRF header succeeds", withHeader === 200, String(withHeader));
    const without = await call({});
    check("same call without the header is 401", without === 401, String(without));

    const other = await context.newPage();
    await other.goto(otherOrigin);
    const blocked = await other.evaluate(
      async ([target, token]) => {
        try {
          const res = await fetch(`${target}/api/memory/search?q=x`, {
            credentials: "include",
            headers: { "x-rh-csrf": token },
          });
          return `readable:${res.status}`;
        } catch (error) {
          return `blocked:${error.name}`;
        }
      },
      [origin, csrf],
    );
    check("cross-origin fetch from another port is blocked by CORS", blocked.startsWith("blocked:"), blocked);
    await other.close();

    const out = await call({ "x-rh-csrf": csrf }, "/api/auth/logout", { method: "POST" });
    check("logout succeeds", out >= 200 && out < 300, String(out));
    const after = await call({ "x-rh-csrf": csrf });
    check("API after logout is 401", after === 401, String(after));
  } finally {
    await browser.close();
  }
  return results;
}

let failed = false;
const other = await startOtherOrigin(PORT + 1);
try {
  for (const name of WANT) {
    const home = mkdtempSync(join(tmpdir(), "rh-browser-"));
    const origin = `http://127.0.0.1:${PORT}`;
    const gateway = spawn(RH, ["local", "--port", String(PORT), "--no-lan"], {
      env: { HOME: home, PATH: process.env.PATH },
      cwd: home,
      stdio: "ignore",
    });
    console.log(`== ${name} (${playwright[name].name()})`);
    try {
      await waitHealthy(origin);
      await runBrowser(name, home, origin, `http://127.0.0.1:${PORT + 1}/`);
    } catch (error) {
      failed = true;
      console.log(`  ERROR ${error.message}`);
    } finally {
      gateway.kill("SIGTERM");
      await sleep(500);
      gateway.kill("SIGKILL");
      rmSync(home, { recursive: true, force: true });
    }
  }
} finally {
  other.close();
}
console.log(failed ? "RESULT: FAIL" : "RESULT: PASS");
process.exit(failed ? 1 : 0);
