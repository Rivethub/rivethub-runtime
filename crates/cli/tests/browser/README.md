# Browser login test

Opt-in. It is not part of `cargo nextest`, CI, or `cargo xtask check`.

It starts a real `rh local` on a throwaway HOME, runs `rh open --no-browser`, and drives a headless browser through the
local login flow: fragment stripped, session cookie flags, CSRF header required, cross-origin fetch blocked by CORS,
and logout.

Playwright is not a repo dependency. Install it in a directory outside the repo:

    mkdir /tmp/rh-pw && cd /tmp/rh-pw && npm init -y && npm i playwright
    npx playwright install chromium            # firefox and webkit also work where the host supports them

Run it with the built binary (ports `PORT` and `PORT+1` must be free):

    RH_BIN=target/debug/rh PLAYWRIGHT_DIR=/tmp/rh-pw RH_PORT=62540 BROWSERS=chromium,firefox,webkit \
      node crates/cli/tests/browser/login-flow.mjs

`CHROMIUM_PATH` points chromium at a system browser instead of the Playwright download. The exit code is 0 only when
every check passes in every browser named in `BROWSERS`.
