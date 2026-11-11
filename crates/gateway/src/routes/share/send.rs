use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use kernel::memory::share;
use kernel::memory::share::desk;
use wire::gateway::share as body;

use super::NO_STORE;
use super::Refusal;
use super::ShareCaller;
use super::ShareJson;
use super::draft_id;
use crate::Gateway;
use crate::auth::FreshLogin;

pub(super) async fn find(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    ShareJson(request): ShareJson<body::FindRequest>,
) -> Result<Json<body::FindReply>, Refusal> {
    let found = gateway
        .share_desk()?
        .find(desk::FindQuery {
            query: request.query,
            since: request.since,
            before: request.before,
            agent: request.agent,
            tag: request.tag,
            limit: request.limit,
        })
        .await?;
    Ok(Json(body::FindReply {
        sessions: found
            .sessions
            .into_iter()
            .map(|session| body::FoundSession {
                id: session.id,
                title: session.title,
                agent: session.agent,
                first: session.first,
                last: session.last,
                messages: session.messages,
                best: session.best,
                hits: session.hits,
                excerpts: session.excerpts,
                imported_from: session.imported_from,
            })
            .collect(),
        dropped: found.dropped,
    }))
}

pub(super) async fn draft(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    ShareJson(request): ShareJson<body::DraftRequest>,
) -> Result<Response, Refusal> {
    let sessions = share::choices(
        request
            .sessions
            .into_iter()
            .map(|chosen| {
                (
                    chosen.id,
                    match chosen.decision {
                        body::SessionDecision::Approve => share::Decision::Approve,
                        body::SessionDecision::Redact => share::Decision::Redact,
                    },
                    chosen.redact.map(|rules| share::RedactRules {
                        terms: rules.terms,
                        drop_tool_outputs: rules.drop_tool_outputs,
                    }),
                )
            })
            .collect(),
    )
    .map_err(|refusal| desk::DeskError::Choices(refusal.to_string()))?;
    let drafted = gateway
        .share_desk()?
        .draft(desk::DraftRequest {
            sessions,
            decide: request.decide.unwrap_or(false),
            block: request.block.unwrap_or(false),
        })
        .await?;
    Ok((StatusCode::CREATED, NO_STORE, Json(report_body(drafted))).into_response())
}

pub(super) async fn report(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    Path(id): Path<String>,
) -> Result<Response, Refusal> {
    let drafted = gateway.share_desk()?.report(draft_id(&id)?).await?;
    Ok((NO_STORE, Json(report_body(drafted))).into_response())
}

pub(super) async fn mark(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    Path(id): Path<String>,
    ShareJson(request): ShareJson<body::MarksRequest>,
) -> Result<Response, Refusal> {
    let marks = request
        .marks
        .into_iter()
        .map(|mark| desk::Mark {
            number: mark.number,
            choice: match mark.choice {
                body::MarkChoice::Keep => desk::FlagChoice::Keep,
                body::MarkChoice::Redact => desk::FlagChoice::Redact,
                body::MarkChoice::Map { term, placeholder } => {
                    desk::FlagChoice::Map { term, placeholder }
                }
            },
            learn: mark.learn.unwrap_or(false),
        })
        .collect();
    let marked = gateway.share_desk()?.mark(draft_id(&id)?, marks).await?;
    let reply = body::MarksReply {
        report: report_body(marked.draft),
        rechecked: marked.rechecked,
        learned: marked.learned,
    };
    Ok((NO_STORE, Json(reply)).into_response())
}

pub(super) async fn send(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    _fresh: FreshLogin,
    Path(id): Path<String>,
    ShareJson(request): ShareJson<body::SendRequest>,
) -> Result<Response, Refusal> {
    let sent = gateway
        .share_desk()?
        .send(
            draft_id(&id)?,
            desk::SendRequest {
                server: request.server.into(),
                signed: request.signed,
                confirm: desk::SendConfirm {
                    sessions: request.confirm.sessions,
                    kept_flags: request.confirm.kept_flags,
                },
            },
        )
        .await?;
    let reply = body::SentReply {
        code: sent.code,
        expires_at: sent.expires_at,
        bytes: sent.bytes,
        signed: sent.signed,
    };
    Ok((NO_STORE, Json(reply)).into_response())
}

pub(super) async fn discard(
    State(gateway): State<Arc<Gateway>>,
    _caller: ShareCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, Refusal> {
    gateway.share_desk()?.discard(draft_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn report_body(draft: desk::Draft) -> body::DraftReport {
    body::DraftReport {
        draft: draft.id.to_string(),
        expires_at: draft.expires_at,
        totals: body::Totals {
            secrets: draft.totals.secrets,
            mapped: draft.totals.mapped,
            redacted: draft.totals.redacted,
            escaped: draft.totals.escaped,
            rechecked: draft.totals.rechecked,
        },
        rules: draft
            .rules
            .into_iter()
            .map(|rule| body::RuleCount {
                rule: rule.rule,
                count: rule.count,
            })
            .collect(),
        sessions: draft
            .sessions
            .into_iter()
            .map(|session| body::DraftSession {
                id: session.id,
                agent: session.agent,
                messages: session.messages,
            })
            .collect(),
        not_scanned: draft.not_scanned,
        best_effort: draft.best_effort,
        decision: draft.decision.map(|decision| body::Decision {
            model: decision.model,
            scored: decision.scored,
            calls: decision.calls,
            trivial: decision.trivial,
            mode: match decision.mode {
                share::DecideMode::Advisory => body::DecideMode::Advisory,
                share::DecideMode::Block => body::DecideMode::Block,
            },
        }),
        flags: draft.flags.into_iter().map(flag_body).collect(),
    }
}

fn flag_body(flag: desk::FlagView) -> body::Flag {
    body::Flag {
        number: flag.number,
        score: flag.score,
        session: flag.session,
        place: flag.place.map(|place| body::FlagPlace {
            session: place.session,
            message: place.message,
            field: place.field,
            more: place.more,
        }),
        length: flag.length,
        text: flag.text.as_str().to_owned(),
        state: match flag.state {
            desk::FlagState::Open => body::FlagState::Open,
            desk::FlagState::Keep => body::FlagState::Keep,
            desk::FlagState::Redact => body::FlagState::Redact,
            desk::FlagState::Map { term, placeholder } => {
                body::FlagState::Map { term, placeholder }
            }
        },
    }
}
