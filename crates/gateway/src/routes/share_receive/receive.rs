use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse as _;
use axum::response::Response;
use kernel::memory::share::receive as port;
use wire::capture::ReceiptVerification;
use wire::gateway::share_receive as body;

use super::NO_STORE;
use super::ReceiveCaller;
use super::ReceiveJson;
use super::Refusal;
use crate::Gateway;
use crate::auth::FreshLogin;
use crate::error::ApiError;

pub(super) async fn inspect(
    State(gateway): State<Arc<Gateway>>,
    _caller: ReceiveCaller,
    ReceiveJson(request): ReceiveJson<body::ReceiveRequest>,
) -> Result<Response, Refusal> {
    let inspected = gateway
        .receive_desk()?
        .inspect(port::ReceiveCode::new(request.code.0))
        .await?;
    Ok((NO_STORE, Json(report(inspected))).into_response())
}

pub(super) async fn import(
    State(gateway): State<Arc<Gateway>>,
    _caller: ReceiveCaller,
    fresh: Result<FreshLogin, ApiError>,
    ReceiveJson(request): ReceiveJson<body::ImportRequest>,
) -> Result<Response, Refusal> {
    let mut seen = BTreeSet::new();
    let mut sessions = Vec::with_capacity(request.sessions.len());
    for choice in &request.sessions {
        let number = choice.n;
        if number == 0 || !seen.insert(number) {
            return Err(Refusal::invalid(
                "each decision names a session number from 1, once",
            ));
        }
        sessions.push(port::SessionChoice {
            number,
            choice: match choice.decision {
                wire::capture::ReceiveDecision::Import => port::Choice::Import,
                wire::capture::ReceiveDecision::Skip => port::Choice::Skip,
                wire::capture::ReceiveDecision::HoldSpans => port::Choice::HoldSpans,
            },
        });
    }
    let imported = gateway
        .receive_desk()?
        .import(
            port::ReceiveCode::new(request.code.0),
            port::ImportRequest {
                sessions,
                force: request.force == Some(true),
                fresh_login: fresh.is_ok(),
            },
        )
        .await?;
    Ok((NO_STORE, Json(reply(imported))).into_response())
}

fn kind(verification: port::Verification) -> ReceiptVerification {
    match verification {
        port::Verification::Verified => ReceiptVerification::Verified,
        port::Verification::Unsigned => ReceiptVerification::Unsigned,
        port::Verification::UnknownKey => ReceiptVerification::UnknownKey,
    }
}

fn signer(signer: port::Signer) -> body::BundleSigner {
    body::BundleSigner {
        key: signer.key,
        fingerprint: signer.fingerprint,
    }
}

fn row(row: port::FindingRow) -> body::FindingRow {
    body::FindingRow {
        event: row.event,
        field: row.field,
        rule: row.rule,
        family: match row.family {
            port::FindingFamily::Secret => body::FindingFamily::Secret,
            port::FindingFamily::Injection => body::FindingFamily::Injection,
        },
        kind: row.kind,
        count: row.count,
        excerpt: row.excerpt,
    }
}

fn session(session: port::Inbound) -> body::InboundSession {
    let allowed = if session.findings == 0 {
        vec![
            wire::capture::ReceiveDecision::Import,
            wire::capture::ReceiveDecision::Skip,
        ]
    } else if session.secret {
        vec![
            wire::capture::ReceiveDecision::Skip,
            wire::capture::ReceiveDecision::HoldSpans,
        ]
    } else {
        vec![
            wire::capture::ReceiveDecision::Import,
            wire::capture::ReceiveDecision::Skip,
            wire::capture::ReceiveDecision::HoldSpans,
        ]
    };
    body::InboundSession {
        n: session.number,
        author: session.author,
        title: session.title,
        agent: session.agent,
        messages: session.messages,
        created_at: session.created_at,
        updated_at: session.updated_at,
        findings: session.findings,
        rows: session.rows.into_iter().map(row).collect(),
        secret: session.secret,
        allowed,
    }
}

fn report(inspected: port::Inspected) -> body::ReceiveReport {
    let held = inspected.author_findings > 0
        || inspected
            .sessions
            .iter()
            .any(|session| session.findings > 0);
    body::ReceiveReport {
        bundle: inspected.bundle,
        server: inspected.server,
        downloaded: inspected.downloaded,
        author: inspected.author,
        node: inspected.node,
        verification: kind(inspected.verification),
        reason: inspected.reason,
        signer: inspected.signer.map(signer),
        sessions: inspected.sessions.into_iter().map(session).collect(),
        author_findings: inspected.author_findings,
        author_rows: inspected.author_rows.into_iter().map(row).collect(),
        summary: inspected.summary,
        held,
    }
}

fn reply(imported: port::Imported) -> body::ImportReply {
    body::ImportReply {
        bundle: imported.bundle,
        received_at: imported.received_at,
        author: imported.author,
        verification: kind(imported.verification),
        signer: imported.signer.map(signer),
        imported: imported
            .sessions
            .into_iter()
            .map(|session| body::ImportedSession {
                n: session.number,
                key: session.key,
                verification: kind(session.verification),
                verdict: match session.verdict {
                    port::Verdict::Clean => body::ImportVerdict::Clean,
                    port::Verdict::Import => body::ImportVerdict::Import,
                    port::Verdict::HoldSpans => body::ImportVerdict::HoldSpans,
                },
                inserted: session.inserted,
                skipped: session.skipped,
            })
            .collect(),
        skipped: imported.skipped,
    }
}
