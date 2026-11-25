use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use kernel::memory::share::desk::BundleName;
use kernel::memory::share::desk::Delivery;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::FlagChoice;
use kernel::memory::share::desk::FlagState;
use kernel::memory::share::desk::Mark;
use kernel::memory::share::desk::PushRequest;
use kernel::memory::share::desk::SendConfirm;
use kernel::memory::share::desk::ServerKind;
use kernel::memory::share::desk::ShareDesk as _;
use rivethub::share_desk::NodeDesk;

use crate::sessions::Node;
use crate::sessions::RISKY;
use crate::sessions::TestResult;
use crate::sessions::call;
use crate::sessions::chosen;
use crate::sessions::sessions;
use crate::share_desk::Drops;
use crate::share_desk::Pushes;
use crate::share_desk::add_hub;
use crate::share_desk::desk;
use crate::share_desk::hub_capture;
use crate::share_desk::hub_me;
use crate::share_desk::model;
use crate::share_desk::send;
use crate::share_desk::sign_in;
use crate::share_desk::team_login;
use crate::share_desk::upload;

fn share_dir(node: &Node) -> PathBuf {
    node.home.path().join(".local/share/rivethub/share")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Review {
    Off,
    Advise,
    Block,
}

async fn sanitize(node: &Node, ids: &[&str], out: &str, review: Review) -> TestResult {
    let chosen: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"id":"{id}","decision":"approve"}}"#))
        .collect();
    let (refused, reply) = call(
        &node.server(false)?,
        "session_sanitize",
        &format!(
            r#"{{"sessions":[{}],"out":"{out}","decide":{},"block":{}}}"#,
            chosen.join(","),
            review != Review::Off,
            review == Review::Block
        ),
    )
    .await?;
    assert!(!refused, "{reply}");
    Ok(())
}

fn raw_copy(node: &Node, from: &str, to: &str) -> TestResult {
    let share = share_dir(node);
    let bundle = capture::bundle::parse(&std::fs::read_to_string(share.join(from))?)?;
    let raw = capture::bundle::seal(
        bundle.manifest.exported_by,
        bundle.manifest.exported_at,
        wire::capture::Sanitization::Raw,
        bundle.sessions,
    )?;
    std::fs::write(share.join(to), raw.text)?;
    Ok(())
}

fn name(text: &str) -> TestResult<BundleName> {
    Ok(text.parse()?)
}

async fn listed(desk: &NodeDesk) -> TestResult<Vec<(String, u32, u32)>> {
    let mut names: Vec<(String, u32, u32)> = desk
        .bundles()
        .await?
        .into_iter()
        .map(|bundle| (bundle.name.to_string(), bundle.sessions, bundle.open_flags))
        .collect();
    names.sort();
    Ok(names)
}

#[tokio::test]
async fn only_sanitized_bundles_are_listed_as_ready() -> TestResult {
    let model = model().await?;
    let dana = Node::new("dana", &format!("http://{}", model.addr), &sessions()).await?;
    let (alpha, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    assert_eq!(listed(&desk).await?, Vec::new(), "no share folder yet");

    sanitize(&dana, &[&alpha, &gamma], "ready.json", Review::Off).await?;
    sanitize(&dana, &[&gamma], "flagged.json", Review::Advise).await?;
    raw_copy(&dana, "ready.json", "raw.json")?;
    let share = share_dir(&dana);
    std::fs::write(share.join("junk.json"), "not a bundle")?;
    std::fs::write(share.join("notes.txt"), "hello")?;
    std::fs::copy(share.join("ready.json"), share.join("has space.json"))?;
    std::os::unix::fs::symlink(share.join("ready.json"), share.join("linked.json"))?;
    assert!(share.join("ready.report.md").exists());
    assert!(share.join("flagged.review.local.json").exists());

    assert_eq!(
        listed(&desk).await?,
        [("flagged".to_owned(), 1, 1), ("ready".to_owned(), 2, 0)]
    );
    let ready = desk
        .bundles()
        .await?
        .into_iter()
        .find(|bundle| bundle.name.as_str() == "ready")
        .ok_or("ready is listed")?;
    assert_eq!(
        ready.bytes,
        std::fs::metadata(share.join("ready.json"))?.len()
    );
    let stamped = capture::bundle::parse(&std::fs::read_to_string(share.join("ready.json"))?)?;
    let wire::capture::Sanitization::Sanitized(stamp) = stamped.manifest.sanitization else {
        return Err("ready.json is sanitized".into());
    };
    assert_eq!(ready.created_at, stamp.at);

    for refused in ["raw", "junk", "linked", "missing", "ready.report"] {
        let drafted = desk.draft_bundle(name(refused)?).await;
        assert!(
            matches!(drafted, Err(DeskError::UnknownBundle)),
            "{refused}: {drafted:?}"
        );
    }
    let flagged = desk.draft_bundle(name("flagged")?).await?;
    assert_eq!(flagged.open_flags(), 1);
    let flag = flagged.flags.first().ok_or("the flag is shown")?;
    assert_eq!(flag.state, FlagState::Open);
    assert!(flag.score > 0.0, "{}", flag.score);
    assert!(flag.text.as_str().contains(RISKY), "{flag:?}");
    assert!(flagged.decision.is_some());

    std::fs::copy(share.join("flagged.json"), share.join("orphan.json"))?;
    let orphan = desk.draft_bundle(name("orphan")?).await;
    assert!(
        matches!(orphan, Err(DeskError::BundleFlags { flags: 1 })),
        "a bundle without its review files cannot be reviewed: {orphan:?}"
    );
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_listed_bundle_pushes_to_a_hub_and_only_the_bundle_file_goes() -> TestResult {
    let captured = Pushes::default();
    let hub = testkit::stub_server(
        axum::Router::new()
            .route("/api/me", axum::routing::get(hub_me))
            .route("/api/capture", axum::routing::post(hub_capture))
            .with_state(captured.clone()),
    )
    .await?;
    let host = hub.addr.to_string();
    let dana = Node::new("dana", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sanitize(&dana, &[&alpha, &gamma], "ready.json", Review::Off).await?;
    add_hub(&dana, &host)?;
    team_login(&dana, &host)?;
    let share = share_dir(&dana);
    let on_disk = capture::bundle::parse(&std::fs::read_to_string(share.join("ready.json"))?)?;

    let draft = desk.draft_bundle(name("ready")?).await?;
    assert_eq!(draft.sessions.len(), 2);
    assert_eq!(draft.open_flags(), 0);
    let wrong = desk
        .push(
            draft.id,
            PushRequest {
                hub: "team".to_owned().into(),
                confirm: SendConfirm {
                    sessions: 1,
                    kept_flags: 0,
                },
            },
        )
        .await;
    assert!(matches!(wrong, Err(DeskError::Counts { .. })), "{wrong:?}");
    assert!(
        share.join("ready.json").exists(),
        "a refused push keeps the bundle"
    );

    let delivered = desk
        .push(
            draft.id,
            PushRequest {
                hub: "team".to_owned().into(),
                confirm: SendConfirm {
                    sessions: 2,
                    kept_flags: 0,
                },
            },
        )
        .await?;
    assert_eq!((delivered.user.as_str(), delivered.sessions), ("fay", 2));
    let seen = captured.0.lock().clone();
    let titles: Vec<Option<String>> = seen.iter().map(|(_, batch)| batch.title.clone()).collect();
    let sent: Vec<Option<String>> = on_disk
        .sessions
        .iter()
        .map(|session| session.batch.title.clone())
        .collect();
    assert_eq!(titles, sent, "the hub got the bundle as it was on disk");
    assert!(
        !share.join("ready.json").exists(),
        "the shared bundle is removed"
    );
    assert!(
        share.join("ready.report.md").exists(),
        "the local report stays"
    );
    assert_eq!(listed(&desk).await?, Vec::new());
    assert!(matches!(
        desk.report(draft.id).await,
        Err(DeskError::UnknownDraft)
    ));
    hub.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_withheld_flag_in_a_bundle_is_marked_on_the_desk_then_pushed() -> TestResult {
    let model = model().await?;
    let captured = Pushes::default();
    let hub = testkit::stub_server(
        axum::Router::new()
            .route("/api/me", axum::routing::get(hub_me))
            .route("/api/capture", axum::routing::post(hub_capture))
            .with_state(captured.clone()),
    )
    .await?;
    let host = hub.addr.to_string();
    let dana = Node::new("dana", &format!("http://{}", model.addr), &sessions()).await?;
    let (_, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sanitize(&dana, &[&gamma], "held.json", Review::Block).await?;
    add_hub(&dana, &host)?;
    team_login(&dana, &host)?;
    let share = share_dir(&dana);
    let confirm = PushRequest {
        hub: "team".to_owned().into(),
        confirm: SendConfirm {
            sessions: 1,
            kept_flags: 0,
        },
    };

    let draft = desk.draft_bundle(name("held")?).await?;
    assert_eq!(draft.open_flags(), 1);
    let refused = desk.push(draft.id, confirm.clone()).await;
    assert!(
        matches!(refused, Err(DeskError::OpenFlags { open: 1 })),
        "{refused:?}"
    );
    let marked = desk
        .mark(
            draft.id,
            vec![Mark {
                number: 1,
                choice: FlagChoice::Redact,
                learn: false,
            }],
        )
        .await?;
    assert_eq!(marked.draft.open_flags(), 0);
    desk.push(draft.id, confirm).await?;

    let seen = captured.0.lock().clone();
    let text = serde_json::to_string(&seen.iter().map(|(_, batch)| batch).collect::<Vec<_>>())?;
    assert!(text.contains("[REDACTED:flagged]"), "{text}");
    assert!(!text.contains(RISKY), "{text}");
    assert!(!text.contains("[FLAGGED:"), "{text}");
    assert!(
        !share.join("held.json").exists(),
        "the shared bundle is removed"
    );
    assert!(share.join("held.review.local.json").exists());
    model.close().await?;
    hub.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_bundle_sent_to_a_drop_box_is_removed_unless_it_changed() -> TestResult {
    let drops = Drops::default();
    let drop_box = testkit::stub_server(
        axum::Router::new()
            .route("/api/drops", axum::routing::post(upload))
            .with_state(drops.clone()),
    )
    .await?;
    let host = drop_box.addr.to_string();
    let dana = Node::new("dana", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sign_in(&dana, &host)?;
    sanitize(&dana, &[&alpha], "one.json", Review::Off).await?;
    sanitize(&dana, &[&gamma], "two.json", Review::Off).await?;
    let share = share_dir(&dana);

    let one = desk.draft_bundle(name("one")?).await?;
    drops
        .0
        .lock()
        .push(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(desk.send(one.id, send(&host, 1, 0)).await.is_err());
    assert!(
        share.join("one.json").exists(),
        "a failed send keeps the bundle"
    );
    desk.send(one.id, send(&host, 1, 0)).await?;
    assert!(!share.join("one.json").exists());

    let two = desk.draft_bundle(name("two")?).await?;
    let replaced = std::fs::read(share.join("two.json"))?;
    std::fs::remove_file(share.join("two.json"))?;
    std::fs::write(share.join("two.json"), [&replaced[..], b"\n\n"].concat())?;
    desk.send(two.id, send(&host, 1, 0)).await?;
    assert!(
        share.join("two.json").exists(),
        "a file replaced after the draft is not the one that was sent"
    );
    drop_box.close().await?;
    Ok(())
}

fn logins(node: &Node, hosts: &[(&str, &str)]) -> TestResult {
    let path = node
        .home
        .path()
        .join(".local/state/rivethub/share-logins.json");
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    let entries: Vec<String> = hosts
        .iter()
        .map(|(host, login)| format!(r#""{host}":{login}"#))
        .collect();
    std::fs::write(&path, format!(r#"{{"logins":{{{}}}}}"#, entries.join(",")))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn policy(node: &Node, allowed: &[&str]) -> TestResult {
    let config = node.home.path().join(".rivethub/config.yaml");
    let mut list = String::from("memory:\n  share:\n    allowed_servers:\n");
    for host in allowed {
        list.extend(["      - ", host, "\n"]);
    }
    let text = std::fs::read_to_string(&config)?.replacen("memory:\n", &list, 1);
    std::fs::write(&config, text + "cloud:\n  disabled: true\n")?;
    Ok(())
}

type Shown = (String, ServerKind, Delivery);

async fn origins(desk: &NodeDesk) -> TestResult<(Vec<Shown>, bool)> {
    let servers = desk.servers().await?;
    let mut shown: Vec<Shown> = servers
        .servers
        .into_iter()
        .map(|server| (server.origin.to_string(), server.kind, server.delivery))
        .collect();
    shown.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((shown, servers.cloud_disabled))
}

fn push(hub: &str) -> Delivery {
    Delivery::Push {
        hub: hub.to_owned(),
    }
}

#[tokio::test]
async fn servers_the_share_policy_refuses_are_not_offered() -> TestResult {
    let dana = Node::new("dana", "http://127.0.0.1:9", &sessions()).await?;
    let desk = desk(&dana, false)?;
    let team = "127.0.0.1:7443";
    add_hub(&dana, team)?;
    let device = r#"{"access_token":"rhca_x","refresh_token":"rhcr_x","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"}"#;
    let member = r#"{"team_token":"t","user":"fay"}"#;
    logins(
        &dana,
        &[
            ("rivethub.cloud", device),
            (team, member),
            ("other.example:8443", member),
            ("hub.example:9443", device),
        ],
    )?;
    assert_eq!(
        origins(&desk).await?,
        (
            vec![
                (team.to_owned(), ServerKind::Team, push("team")),
                (
                    "hub.example:9443".to_owned(),
                    ServerKind::Cloud,
                    push("hub.example:9443")
                ),
                (
                    "other.example:8443".to_owned(),
                    ServerKind::Team,
                    push("other.example:8443")
                ),
                (
                    "rivethub.cloud".to_owned(),
                    ServerKind::Cloud,
                    Delivery::Code
                ),
            ],
            false
        )
    );

    policy(&dana, &[team])?;
    assert_eq!(
        origins(&desk).await?,
        (
            vec![(team.to_owned(), ServerKind::Team, push("team"))],
            true
        )
    );
    Ok(())
}

#[tokio::test]
async fn text_outside_the_digest_keeps_a_bundle_from_being_ready() -> TestResult {
    let dana = Node::new("dana", "http://127.0.0.1:9", &sessions()).await?;
    let (alpha, _, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sanitize(&dana, &[&alpha], "ready.json", Review::Off).await?;
    let share = share_dir(&dana);
    let text = std::fs::read_to_string(share.join("ready.json"))?;
    let leak = format!(r#""leak":"{RISKY} unsanitized","#);
    for (at, anchor) in [
        ("top", r#"{"manifest":{"#),
        ("manifest", r#""exported_at":"#),
        ("session", r#""sessions":[{"#),
        ("batch", r#""batch":{"#),
    ] {
        let (before, after) = text.split_once(anchor).ok_or(anchor)?;
        let (open, rest) = if anchor.ends_with('{') {
            (anchor, "")
        } else {
            ("", anchor)
        };
        let padded = format!("{before}{open}{leak}{rest}{after}");
        let with = format!("{at}.json");
        std::fs::write(share.join(&with), &padded)?;
        assert!(
            capture::bundle::parse(&padded).is_ok(),
            "{at}: the digest does not cover the extra field"
        );
        let drafted = desk.draft_bundle(name(at)?).await;
        assert!(
            matches!(drafted, Err(DeskError::UnknownBundle)),
            "{at}: {drafted:?}"
        );
    }
    assert_eq!(listed(&desk).await?, [("ready".to_owned(), 1, 0)]);
    Ok(())
}

fn copied(share: &std::path::Path, from: &str, to: &str) -> TestResult {
    for file in ["json", "report.md", "review.local.json"] {
        std::fs::copy(
            share.join(format!("{from}.{file}")),
            share.join(format!("{to}.{file}")),
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn review_files_that_are_links_are_refused() -> TestResult {
    let model = model().await?;
    let dana = Node::new("dana", &format!("http://{}", model.addr), &sessions()).await?;
    let (_, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sanitize(&dana, &[&gamma], "flagged.json", Review::Advise).await?;
    let share = share_dir(&dana);
    let elsewhere = dana.home.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere)?;
    desk.draft_bundle(name("flagged")?).await?;

    for sidecar in ["report.md", "review.local.json"] {
        let linked = format!("linked-{}", sidecar.replace('.', "-"));
        copied(&share, "flagged", &linked)?;
        let file = format!("{linked}.{sidecar}");
        std::fs::rename(share.join(&file), elsewhere.join(&file))?;
        std::os::unix::fs::symlink(elsewhere.join(&file), share.join(&file))?;
        let drafted = desk.draft_bundle(name(&linked)?).await;
        assert!(
            matches!(drafted, Err(DeskError::Refused(_))),
            "a linked {sidecar} is refused: {drafted:?}"
        );
    }
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_local_review_file_that_lost_its_seal_is_refused() -> TestResult {
    let model = model().await?;
    let dana = Node::new("dana", &format!("http://{}", model.addr), &sessions()).await?;
    let (_, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sanitize(&dana, &[&gamma], "flagged.json", Review::Advise).await?;
    let share = share_dir(&dana);
    copied(&share, "flagged", "forged")?;
    let local = share.join("forged.review.local.json");
    let mut review: capture::sanitize::LocalReview =
        serde_json::from_slice(&std::fs::read(&local)?)?;
    let flag = review
        .flags
        .first_mut()
        .ok_or("the local review holds the flag's text")?;
    "forged text to show".clone_into(&mut flag.text);
    std::fs::write(&local, serde_json::to_vec(&review)?)?;

    desk.draft_bundle(name("flagged")?).await?;
    let drafted = desk.draft_bundle(name("forged")?).await;
    assert!(
        matches!(drafted, Err(DeskError::Refused(_))),
        "a local review file without its seal is refused: {drafted:?}"
    );
    model.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_bundle_with_kept_flags_needs_its_review_files_and_the_kept_count() -> TestResult {
    let model = model().await?;
    let captured = Pushes::default();
    let hub = testkit::stub_server(
        axum::Router::new()
            .route("/api/me", axum::routing::get(hub_me))
            .route("/api/capture", axum::routing::post(hub_capture))
            .with_state(captured.clone()),
    )
    .await?;
    let host = hub.addr.to_string();
    let dana = Node::new("dana", &format!("http://{}", model.addr), &sessions()).await?;
    let (_, gamma, _) = chosen(&dana)?;
    let desk = desk(&dana, false)?;
    sanitize(&dana, &[&gamma], "flagged.json", Review::Advise).await?;
    let (_, env) = dana.loaded(false)?;
    add_hub(&dana, &host)?;
    team_login(&dana, &host)?;
    let share = share_dir(&dana);
    let key = capture::sanitize::LocalKey::load_or_create(&capture::share::key_path(&env)?)?;
    let bundle = capture::bundle::parse(&std::fs::read_to_string(share.join("flagged.json"))?)?;
    let local: capture::sanitize::LocalReview =
        serde_json::from_slice(&std::fs::read(share.join("flagged.review.local.json"))?)?;
    let flags = capture::sanitize::ReportFlags::from_choices(
        &local,
        &std::collections::BTreeMap::new(),
        &[Mark {
            number: 1,
            choice: FlagChoice::Keep,
            learn: false,
        }],
    )?;
    let kept = capture::sanitize::resolve(&bundle, &flags, &local, &key)?;
    std::fs::write(share.join("kept.json"), &kept.sealed.text)?;
    assert_eq!(listed(&desk).await?.len(), 2, "a kept flag is not open");

    let drafted = desk.draft_bundle(name("kept")?).await;
    assert!(
        matches!(drafted, Err(DeskError::BundleFlags { .. })),
        "a kept flag cannot be shown without the review files: {drafted:?}"
    );

    for file in ["report.md", "review.local.json"] {
        std::fs::copy(
            share.join(format!("flagged.{file}")),
            share.join(format!("kept.{file}")),
        )?;
    }
    let draft = desk.draft_bundle(name("kept")?).await?;
    let shown = draft.flags.first().ok_or("the kept flag is shown")?;
    assert_eq!(shown.state, FlagState::Keep);
    assert!(shown.text.as_str().contains(RISKY), "{shown:?}");
    let confirm = |kept_flags| PushRequest {
        hub: "team".to_owned().into(),
        confirm: SendConfirm {
            sessions: 1,
            kept_flags,
        },
    };
    let unconfirmed = desk.push(draft.id, confirm(0)).await;
    assert!(
        matches!(unconfirmed, Err(DeskError::Counts { kept: 1, .. })),
        "{unconfirmed:?}"
    );
    desk.push(draft.id, confirm(1)).await?;
    let seen = captured.0.lock().clone();
    let text = serde_json::to_string(&seen.iter().map(|(_, batch)| batch).collect::<Vec<_>>())?;
    assert!(text.contains(RISKY), "the kept text was confirmed and sent");
    model.close().await?;
    hub.close().await?;
    Ok(())
}
