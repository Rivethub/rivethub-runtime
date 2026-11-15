use std::path::Path;

use crate::common;

const RECORDING: &str = "gateway/http-plain.json";

const REPLAYED: &[&str] = &[
    "plain-main/healthz-get",
    "plain-main/healthz-post",
    "plain-main/healthz-head",
    "plain-main/healthz-delete",
    "plain-main/healthz-query",
    "plain-main/healthz-absolute-form",
    "plain-main/healthz-http10",
    "plain-main/healthz-http10-no-host",
    "plain-main/options-healthz",
    "plain-main/options-healthz-same-origin",
    "plain-main/options-api",
    "plain-main/options-api-preflight-app-bundle",
    "plain-main/options-api-disallowed-origin",
    "plain-main/options-unknown-path",
    "plain-main/options-asterisk",
    "plain-main/remote-plain-options",
    "plain-main/origin-healthz-disallowed",
    "plain-main/origin-empty",
    "plain-main/host-evil-healthz",
    "plain-main/host-rebinding-same-origin",
    "plain-main/remote-plain-healthz",
    "plain-main/remote-plain-foreign-host",
    "plain-main/not-found-file",
    "plain-main/not-found-post",
    "plain-main/not-found-api",
    "plain-main/static-root",
    "plain-main/static-index",
    "plain-main/static-index-head",
    "plain-main/static-index-query",
    "plain-main/static-hashed-asset",
    "plain-main/static-plain-asset",
    "plain-main/static-nine-char-suffix",
    "plain-main/static-css",
    "plain-main/static-svg",
    "plain-main/static-json",
    "plain-main/static-png-binary",
    "plain-main/static-no-extension-file",
    "plain-main/static-subdir",
    "plain-main/static-subdir-slash",
    "plain-main/static-spa-fallback",
    "plain-main/static-spa-deep",
    "plain-main/static-missing-with-extension",
    "plain-main/static-encoded-dotdot",
    "plain-main/static-post",
    "plain-main/static-api-prefix",
    "plain-main/alias-uppercase",
    "plain-main/alias-api-events-trailing-slash",
    "plain-main/alias-api-events-unknown-child",
    "plain-redirect/redirect-get-root",
    "plain-redirect/redirect-get-root-query",
    "plain-redirect/redirect-head-root",
    "plain-redirect/redirect-post-root",
    "plain-redirect/redirect-same-origin",
    "plain-redirect/redirect-index",
    "plain-redirect/redirect-remote-unauthorized",
    "plain-nostatic/nostatic-spa",
];

const INDEX: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>golden hub</title></head><body>golden index</body></html>\n";
const PIXEL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 248, 207, 80, 15, 0, 3,
    134, 1, 128, 90, 52, 125, 107, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

fn write_static(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root.join("assets"))?;
    std::fs::create_dir_all(root.join("sub"))?;
    let files: [(&str, &[u8]); 12] = [
        ("index.html", INDEX.as_bytes()),
        ("sub/index.html", b"<!doctype html>\n<p>sub index</p>\n"),
        (
            "assets/app-abcdef12.js",
            b"console.log(\"hashed asset app-abcdef12\")\n",
        ),
        ("assets/plain.js", b"console.log(\"plain asset\")\n"),
        (
            "assets/app-abcdef123.js",
            b"console.log(\"nine-char suffix is not a content hash\")\n",
        ),
        ("style.css", b"body { color: #222; }\n"),
        (
            "logo.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>\n",
        ),
        ("manifest.json", b"{\"name\":\"golden\"}\n"),
        ("pixel.png", PIXEL_PNG),
        ("notes.xyz", b"unknown extension\n"),
        ("IMAGE.PNG", b"upper-case extension, not a real png\n"),
        ("LICENSE", b"golden fixture file without an extension\n"),
    ];
    for (name, bytes) in files {
        std::fs::write(root.join(name), bytes)?;
    }
    Ok(())
}

struct Apps {
    plain: axum::Router,
    statik: axum::Router,
    redirect: axum::Router,
}

impl Apps {
    fn new(static_dir: &Path) -> Self {
        let mut served = common::settings();
        served.static_dir = Some(static_dir.to_path_buf());
        let mut redirected = served.clone();
        redirected.root_redirect = Some("/wiki".to_owned());
        Self {
            plain: common::gateway_with(common::settings()).router(false),
            statik: common::gateway_with(served).router(false),
            redirect: common::gateway_with(redirected).router(false),
        }
    }

    fn serving(&self, id: &str) -> &axum::Router {
        if id.starts_with("plain-redirect/") {
            &self.redirect
        } else if id.starts_with("plain-main/static-") || id.starts_with("plain-main/alias-") {
            &self.statik
        } else {
            &self.plain
        }
    }
}

#[test]
fn recorded_exchanges_are_answered_the_same() -> Result<(), Box<dyn std::error::Error>> {
    let static_dir = tempfile::tempdir()?;
    write_static(static_dir.path())?;
    let apps = Apps::new(static_dir.path());
    let cases = testkit::http::cases(&testkit::recording(RECORDING))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let masks = testkit::Masks {
        port: "5174".to_owned(),
        hostname: Some("gold-host".to_owned()),
        tmp: Some("/tmp/golden".to_owned()),
    };
    let mut failures = Vec::new();
    for id in REPLAYED {
        let mut case = cases
            .iter()
            .find(|case| case.id == *id)
            .ok_or_else(|| format!("{id} is not in {RECORDING}"))?
            .clone();
        common::expect_cors(&mut case);
        let app = apps.serving(id).clone();
        let outcome = testkit::http::replay(&case, &masks, |request| {
            match runtime.block_on(common::answer(app, request.clone())) {
                Ok(response) => response,
                Err(error) => testkit::Response::bytes(599, "text/plain", error),
            }
        });
        if let Err(mismatch) = outcome {
            failures.push(format!("{}: {}", mismatch.id, mismatch.detail));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
