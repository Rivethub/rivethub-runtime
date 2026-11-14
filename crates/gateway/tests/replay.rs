#[expect(
    dead_code,
    reason = "the harness module is included whole by every test binary and this one uses a subset"
)]
#[path = "it/common/mod.rs"]
mod common;

#[path = "it/common/fixture.rs"]
mod fixture;

use libtest_mimic::Arguments;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use libtest_mimic::run;
use std::path::PathBuf;
use testkit::Mismatch;

const RECORDING: &str = "gateway/http-plain.json";

const CLAIMS: &[&str] = &[
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
    "plain-main/host-evil-healthz",
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

const OUT_OF_SCOPE: &[(&str, &str, &str)] = &[
    ("plain-main", "healthz-trailing-slash", "2"),
    ("plain-main", "healthz-percent-encoded", "2"),
    ("plain-main", "healthz-double-slash", "2"),
    ("plain-main", "healthz-http11-no-host", "transport"),
    ("plain-main", "malformed-request-line", "transport"),
    ("plain-main", "origin-none", "2"),
    ("plain-main", "origin-null", "2"),
    ("plain-main", "origin-unparseable", "2"),
    ("plain-main", "origin-empty", "2"),
    ("plain-main", "origin-same", "2"),
    ("plain-main", "origin-same-other-scheme", "2"),
    ("plain-main", "origin-same-with-path", "2"),
    ("plain-main", "origin-same-uppercase", "2"),
    ("plain-main", "origin-default-port", "2"),
    ("plain-main", "origin-port-mismatch", "2"),
    ("plain-main", "origin-app-bundle", "2"),
    ("plain-main", "origin-app-bundle-upper", "2"),
    ("plain-main", "origin-allowed-env", "2"),
    ("plain-main", "origin-allowed-env-default-port", "2"),
    ("plain-main", "origin-mesh-peer-flipped-scheme", "2"),
    ("plain-main", "origin-mesh-peer-explicit-scheme-only", "2"),
    ("plain-main", "origin-disallowed", "2"),
    ("plain-main", "origin-disallowed-repeat", "2"),
    ("plain-main", "host-evil", "2"),
    ("plain-main", "host-evil-with-port", "2"),
    ("plain-main", "host-rebinding-same-origin", "2"),
    ("plain-main", "host-allowed-env", "2"),
    ("plain-main", "host-dot-localhost", "2"),
    ("plain-main", "host-localhost-upper", "2"),
    ("plain-main", "host-ipv6-loopback", "2"),
    ("plain-main", "host-empty", "2"),
    ("plain-main", "remote-plain-api", "2"),
    ("plain-main", "remote-plain-html", "2"),
    (
        "plain-main",
        "static-term-nested",
        "terminal lane owns the disabled handler",
    ),
    ("plain-main", "static-audio-nested", "removed"),
    (
        "plain-main",
        "static-uppercase-extension",
        "mime guessing is case-insensitive; the recording pins a case-sensitive octet-stream quirk",
    ),
    (
        "plain-main",
        "static-unknown-extension",
        "mime_guess maps .xyz to chemical/x-xyz; the recording pins an octet-stream default",
    ),
    (
        "plain-main",
        "static-dotdot",
        "parent segments are refused; the recording serves the SPA after collapsing dot-dot",
    ),
    (
        "plain-main",
        "ws-path-http",
        "the session route owns GET /ws",
    ),
    (
        "plain-main",
        "alias-api-events-ws-http",
        "the session route owns the rewritten /ws path",
    ),
    ("plain-main", "mesh-empty-object", "cut"),
    (
        "plain-nostatic",
        "nostatic-ws-get",
        "the session route owns GET /ws",
    ),
    ("plain-nostatic", "nostatic-root", "no-app-page"),
    ("plain-nostatic", "nostatic-root-html", "no-app-page"),
    ("plain-main", "event-invalid-json", "2"),
    ("plain-main", "event-empty-body", "2"),
    ("plain-main", "event-json-string", "2"),
    ("plain-main", "event-wrong-version", "2"),
    ("plain-main", "event-unknown-type", "2"),
    ("plain-main", "event-bad-ts", "2"),
    ("plain-main", "event-valid-session-start", "2"),
    ("plain-main", "event-valid-chunked", "2"),
    ("plain-main", "event-valid-expect-continue", "2"),
    ("plain-main", "event-reserialize", "2"),
    ("plain-main", "event-get", "2"),
    ("plain-main", "events-not-array", "2"),
    ("plain-main", "events-empty-array", "2"),
    ("plain-main", "events-101", "2"),
    ("plain-main", "events-100", "2"),
    ("plain-main", "events-bad-element-2", "2"),
    ("plain-main", "state-after-rejected-batch", "2"),
    ("plain-main", "events-valid-batch", "2"),
    ("plain-main", "event-canonical-uuid-room", "2"),
    ("plain-main", "sessions-list", "2"),
    ("plain-main", "state-known", "2"),
    ("plain-main", "state-reserialized", "2"),
    ("plain-main", "state-unknown", "2"),
    ("plain-main", "state-missing-param", "2"),
    ("plain-main", "state-empty-param", "2"),
    ("plain-main", "state-canonical-id", "2"),
    ("plain-main", "state-duplicate-param", "2"),
    ("plain-main", "state-post", "2"),
    ("plain-main", "healthz-after-ingest", "2"),
    (
        "plain-main",
        "alias-api-events",
        "events lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-events-event",
        "events lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-events-sessions",
        "events lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-events-state",
        "events lane owns the destination",
    ),
    ("plain-main", "alias-api-mesh", "cut"),
    (
        "plain-main",
        "alias-api-terminal",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-terminal-config",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "term-config",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-terminal-list",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-terminal-inject",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-terminal-harness-sessions",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-terminal-ws-http",
        "terminal lane owns the destination",
    ),
    (
        "plain-main",
        "alias-api-terminal-prefix",
        "terminal lane owns the destination",
    ),
    ("plain-main", "alias-api-audio", "removed"),
    ("plain-main", "alias-api-audio-status", "removed"),
    ("plain-main", "alias-api-audio-health", "removed"),
    ("plain-main", "alias-api-audio-mic-http", "removed"),
    ("plain-main", "delete-session-known", "2"),
    ("plain-main", "delete-session-again", "2"),
    ("plain-main", "delete-session-missing-param", "2"),
    ("plain-main", "delete-session-canonical", "2"),
    ("plain-main", "session-get", "2"),
    ("plain-main", "sessions-after-delete", "2"),
    ("plain-main", "audio-disabled", "removed"),
    ("plain-main", "term-post-disabled", "2"),
    ("plain-main", "body-exactly-256k", "2"),
    ("plain-main", "body-too-large-event", "2"),
    ("plain-main", "body-too-large-events", "2"),
    ("plain-main", "mesh-valid", "cut"),
    ("plain-main", "layout-bad-key-slash", "removed"),
    ("plain-main", "layout-bad-key-empty", "removed"),
    ("plain-main", "layout-bad-key-65", "removed"),
    ("plain-main", "layout-missing", "removed"),
    ("plain-main", "layout-missing-default", "removed"),
    ("plain-main", "layout-post-invalid-json", "removed"),
    ("plain-main", "layout-post-bad-key", "removed"),
    ("plain-main", "layout-post-valid", "removed"),
    ("plain-main", "layout-get-written", "removed"),
    ("plain-main", "layout-post-default", "removed"),
    ("plain-main", "layout-get-fallback", "removed"),
    ("plain-main", "layout-key-dots", "removed"),
    ("plain-main", "layout-put", "removed"),
    ("plain-main", "body-too-large-layout", "removed"),
    ("plain-main", "capabilities", "removed"),
    ("plain-main", "capabilities-post", "removed"),
    ("plain-nostatic", "capabilities-no-mesh", "removed"),
    ("plain-tenancy", "tenancy-ingest-owner-session", "2"),
    ("plain-tenancy", "tenancy-ingest-alice-session", "2"),
    ("plain-tenancy", "tenancy-sessions-owner", "2"),
    ("plain-tenancy", "tenancy-state-own", "2"),
    ("plain-tenancy", "tenancy-state-alice", "2"),
    ("plain-tenancy", "tenancy-state-unknown", "2"),
    ("plain-tenancy", "tenancy-delete-alice", "2"),
    ("plain-tenancy", "tenancy-spoofed-user-header", "2"),
    ("plain-tenancy", "tenancy-unknown-user-token", "2"),
    ("plain-tenancy", "tenancy-empty-user-token", "2"),
    ("plain-tenancy", "tenancy-healthz", "2"),
    ("plain-tenancy", "tenancy-static", "2"),
    ("plain-failclosed", "failclosed-healthz", "2"),
    ("plain-failclosed", "failclosed-sessions", "2"),
    ("plain-failclosed", "failclosed-post-event", "2"),
    ("plain-failclosed", "failclosed-static", "2"),
    ("plain-failclosed", "failclosed-options", "2"),
    ("plain-users-invalid", "users-invalid-sessions", "2"),
    (
        "plain-users-shared-invalid",
        "users-shared-invalid-sessions",
        "2",
    ),
    ("plain-main", "mesh-valid-before-ingest", "cut"),
    ("plain-main", "mesh-no-file", "cut"),
    ("plain-main", "mesh-invalid-json", "cut"),
    ("plain-main", "mesh-root-array", "cut"),
    ("plain-main", "mesh-flat-array", "cut"),
    ("plain-main", "mesh-nodes-number", "cut"),
    ("plain-main", "mesh-post", "cut"),
    ("plain-redirect", "mesh-default-path-missing", "cut"),
    ("plain-main", "remote-plain-pair-redeem", "cut"),
    ("plain-main", "remote-plain-enroll", "cut"),
    ("plain-main", "devices-disabled", "cut"),
    ("plain-main", "devices-enroll-disabled", "cut"),
    ("plain-main", "devices-delete-disabled", "cut"),
    ("plain-main", "phone-pairing-get", "cut"),
    ("plain-main", "phone-pairing-post", "cut"),
    ("plain-main", "phone-pairing-status", "cut"),
    ("plain-main", "phone-pairing-bad-escape", "cut"),
    ("plain-main", "phone-pairing-bad-name", "cut"),
    ("plain-main", "pair-redeem-bogus", "cut"),
    ("plain-main", "pair-redeem-get", "cut"),
    ("plain-tenancy", "tenancy-phone-pairing-owner", "cut"),
    ("plain-main", "static-files-nested", "cut"),
    ("plain-main", "alias-api-files-prefix", "cut"),
    ("plain-main", "files-disabled", "cut"),
    ("plain-main", "files-bare-path", "cut"),
    ("plain-nostatic", "files-root-missing", "cut"),
    ("plain-nostatic", "files-alias-root-missing", "cut"),
];

const SKIP_REASONS: &[(&str, &str)] = &[
    (
        "cut",
        "B1: rh v0.8 cut this route (CONTRACT note 132); the recording keeps the den's answer",
    ),
    (
        "2",
        "B1/B2/B12: session, tenancy and static-serving scenarios are outside this health, origin and file-root replay slice",
    ),
    (
        "transport",
        "B1: raw HTTP parser behavior is outside this router-level replay",
    ),
    (
        "terminal lane owns the disabled handler",
        "terminal lane owns the disabled handler",
    ),
    (
        "removed",
        "B1: note 90 removed the mic bridge, /layout and /api/capabilities; the recording keeps the den's answers",
    ),
    (
        "mime guessing is case-insensitive; the recording pins a case-sensitive octet-stream quirk",
        "mime guessing is case-insensitive; the recording pins a case-sensitive octet-stream quirk",
    ),
    (
        "mime_guess maps .xyz to chemical/x-xyz; the recording pins an octet-stream default",
        "mime_guess maps .xyz to chemical/x-xyz; the recording pins an octet-stream default",
    ),
    (
        "parent segments are refused; the recording serves the SPA after collapsing dot-dot",
        "parent segments are refused; the recording serves the SPA after collapsing dot-dot",
    ),
    (
        "the session route owns GET /ws",
        "the session route owns GET /ws",
    ),
    (
        "no-app-page",
        "B1: a node with no web app answers GET / with a page, not the recording's JSON 404 (CONTRACT note 172)",
    ),
    (
        "the session route owns the rewritten /ws path",
        "the session route owns the rewritten /ws path",
    ),
    (
        "events lane owns the destination",
        "events lane owns the destination",
    ),
    (
        "terminal lane owns the destination",
        "terminal lane owns the destination",
    ),
];

fn golden(name: &str) -> PathBuf {
    testkit::recording(name)
}

fn masks() -> testkit::Masks {
    testkit::Masks {
        port: "5174".to_string(),
        hostname: Some("gold-host".to_string()),
        tmp: Some("/tmp/golden".to_string()),
    }
}

fn must_pass(outcome: Result<(), Mismatch>) -> Result<(), Failed> {
    match outcome {
        Ok(()) => Ok(()),
        Err(mismatch) => Err(Failed::from(format!(
            "{}: {}",
            mismatch.id, mismatch.detail
        ))),
    }
}

fn main() {
    let args = Arguments::from_args();
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => start(&args, &runtime).exit(),
        Err(error) => run(
            &args,
            vec![Trial::test("gateway dev runtime", move || {
                Err::<(), Failed>(Failed::from(format!("dev test runtime: {error}")))
            })],
        )
        .exit(),
    }
}

struct Apps {
    plain: axum::Router,
    statik: axum::Router,
    redirect: axum::Router,
    nostatic: axum::Router,
}

fn start(args: &Arguments, runtime: &tokio::runtime::Runtime) -> libtest_mimic::Conclusion {
    let handle = runtime.handle().clone();
    let static_dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => return failed(args, format!("static dir: {error}")),
    };
    if let Err(error) = fixture::write_static(static_dir.path()) {
        return failed(args, error);
    }
    let redirect_dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => return failed(args, format!("redirect dir: {error}")),
    };
    let nostatic_dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => return failed(args, format!("nostatic dir: {error}")),
    };
    let apps = match apps(&static_dir, &redirect_dir, &nostatic_dir) {
        Ok(apps) => apps,
        Err(error) => return failed(args, error),
    };
    let mut trials = Vec::new();
    match testkit::http::cases(&golden(RECORDING)) {
        Ok(cases) => {
            for case in cases {
                push_case(&mut trials, case, &apps, &handle);
            }
        }
        Err(error) => trials.push(Trial::test("gateway golden load".to_string(), {
            move || {
                Err(Failed::from(format!(
                    "vendored recording unreadable: {error}"
                )))
            }
        })),
    }
    trials.push(Trial::test(
        "gateway claims are recorded".to_string(),
        check_claims,
    ));
    trials.push(Trial::test(
        "gateway out-of-scope coverage".to_string(),
        check_enumeration,
    ));
    let conclusion = run(args, trials);
    drop(static_dir);
    drop(redirect_dir);
    drop(nostatic_dir);
    conclusion
}

fn apps(
    static_dir: &tempfile::TempDir,
    redirect_dir: &tempfile::TempDir,
    nostatic_dir: &tempfile::TempDir,
) -> Result<Apps, String> {
    let redirect = fixture::redirect_settings(static_dir.path(), redirect_dir.path())?;
    let nostatic = fixture::nostatic_settings(nostatic_dir.path())?;
    Ok(Apps {
        plain: app_for(common::settings()),
        statik: app_for(fixture::static_settings(static_dir.path())),
        redirect: app_for(redirect),
        nostatic: app_for(nostatic),
    })
}

fn app_for(settings: gateway::Settings) -> axum::Router {
    common::gateway_with(settings).router(false)
}

fn push_case(
    trials: &mut Vec<Trial>,
    case: testkit::http::HttpCase,
    apps: &Apps,
    handle: &tokio::runtime::Handle,
) {
    if !CLAIMS.contains(&case.id.as_str()) {
        return;
    }
    match fixture::kind(&case.id) {
        fixture::Kind::Static => {
            trials.push(shared_trial(case, apps.statik.clone(), handle.clone()));
        }
        fixture::Kind::Redirect => {
            trials.push(shared_trial(case, apps.redirect.clone(), handle.clone()));
        }
        fixture::Kind::NoStatic => {
            trials.push(shared_trial(case, apps.nostatic.clone(), handle.clone()));
        }
        fixture::Kind::Plain => trials.push(shared_trial(case, apps.plain.clone(), handle.clone())),
    }
}

fn shared_trial(
    case: testkit::http::HttpCase,
    app: axum::Router,
    handle: tokio::runtime::Handle,
) -> Trial {
    let id = case.id.clone();
    Trial::test(format!("gateway {id}"), move || play(&case, &app, &handle))
}

fn play(
    case: &testkit::http::HttpCase,
    app: &axum::Router,
    handle: &tokio::runtime::Handle,
) -> Result<(), Failed> {
    play_masked(case, app, handle, &masks())
}

fn play_masked(
    case: &testkit::http::HttpCase,
    app: &axum::Router,
    handle: &tokio::runtime::Handle,
    masks: &testkit::Masks,
) -> Result<(), Failed> {
    let case = translated_refusal(case)?;
    must_pass(testkit::http::replay(&case, masks, |request| {
        let request = match fixture::expand(request.clone()) {
            Ok(request) => request,
            Err(error) => {
                return testkit::Response::bytes(599, "text/plain", format!("recipe: {error}"));
            }
        };
        match handle.block_on(common::answer(app.clone(), request)) {
            Ok(response) => response,
            Err(error) => {
                testkit::Response::bytes(599, "text/plain", format!("harness failure: {error}"))
            }
        }
    }))
}

fn translated_refusal(case: &testkit::http::HttpCase) -> Result<testkit::http::HttpCase, Failed> {
    let mut case = case.clone();
    common::expect_cors(&mut case);
    if testkit::http::status_of(&case.response.status_line)
        .map_err(|error| Failed::from(error.to_string()))?
        != 403
    {
        return Ok(case);
    }
    let body = case
        .response
        .body
        .as_deref()
        .ok_or_else(|| Failed::from("missing refusal body"))?;
    let mut error: wire::gateway::GatewayError =
        serde_json::from_str(body).map_err(|error| Failed::from(error.to_string()))?;
    error.error = testkit::translate_recorded_product_names(&error.error);
    error.reason = error
        .reason
        .as_deref()
        .map(testkit::translate_recorded_product_names);
    case.response.body =
        Some(serde_json::to_string(&error).map_err(|error| Failed::from(error.to_string()))?);
    Ok(case)
}

fn failed(args: &Arguments, error: String) -> libtest_mimic::Conclusion {
    run(
        args,
        vec![Trial::test("gateway fixture", move || {
            Err(Failed::from(error))
        })],
    )
}

fn recorded_cases() -> Result<Vec<testkit::http::HttpCase>, Failed> {
    testkit::http::cases(&golden(RECORDING))
        .map_err(|error| Failed::from(format!("vendored recording unreadable: {error}")))
}

fn check_claims() -> Result<(), Failed> {
    let recorded = recorded_cases()?;
    let mut recorded_ids: Vec<String> = recorded.iter().map(|case| case.id.clone()).collect();
    recorded_ids.sort();
    let missing: Vec<&str> = CLAIMS
        .iter()
        .copied()
        .filter(|id| !recorded_ids.iter().any(|recorded| recorded == id))
        .collect();
    if !missing.is_empty() {
        return Err(Failed::from(format!(
            "claimed ids not in the vendored recording: {missing:?}"
        )));
    }
    Ok(())
}

fn check_enumeration() -> Result<(), Failed> {
    for (run, id, code) in OUT_OF_SCOPE {
        if !SKIP_REASONS
            .iter()
            .any(|(key, reason)| key == code && !reason.trim().is_empty())
        {
            return Err(Failed::from(format!(
                "missing exclusion reason for {run}/{id}: {code}"
            )));
        }
    }
    let recorded = recorded_cases()?;
    let mut unaccounted: Vec<String> = Vec::new();
    for case in &recorded {
        let Some((run, id)) = case.id.split_once('/') else {
            return Err(Failed::from(format!("case id without a run: {}", case.id)));
        };
        if CLAIMS.contains(&case.id.as_str()) {
            continue;
        }
        let listed = OUT_OF_SCOPE
            .iter()
            .any(|(listed_run, listed_id, _)| *listed_run == run && *listed_id == id);
        if !listed {
            unaccounted.push(case.id.clone());
        }
    }
    if !unaccounted.is_empty() {
        return Err(Failed::from(format!(
            "recorded cases claimed or listed nowhere: {unaccounted:?}"
        )));
    }
    let mut stale: Vec<&(&str, &str, &str)> = OUT_OF_SCOPE
        .iter()
        .filter(|(run, id, _)| {
            let full = format!("{run}/{id}");
            !CLAIMS.contains(&full.as_str()) && !recorded.iter().any(|case| case.id == full)
        })
        .collect();
    stale.sort_unstable();
    if !stale.is_empty() {
        return Err(Failed::from(format!(
            "out-of-scope entries not in the vendored recording: {:?}",
            stale
                .iter()
                .map(|(run, id, _)| format!("{run}/{id}"))
                .collect::<Vec<_>>()
        )));
    }
    Ok(())
}
