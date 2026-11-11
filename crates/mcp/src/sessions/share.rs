use async_trait::async_trait;
use kernel::memory::share::Consent;
use kernel::memory::share::ShareConsent;
use kernel::memory::share::ShareFailure;
use kernel::memory::share::ShareOutcome;
use kernel::memory::share::SharePreview;
use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::model::CallToolResult;
use rmcp::model::ElicitRequestParams;
use rmcp::model::ElicitationAction;
use rmcp::model::ElicitationSchema;
use rmcp::service::ElicitationMode;
use serde::Deserialize;

use super::Sessions;
use super::Share;
use super::files::Kind;
use super::noun;
use crate::Error;

const SHARE: &str = "share";
const FLAGS: &str = "unreviewed_flags";
const NO_ELICITATION: &str = "refused: this client cannot ask the user to confirm a share, so nothing was uploaded; ask the user to run `rh memory share <path>` in a terminal";
const TITLES_SHOWN: usize = 20;

#[derive(Deserialize)]
struct Answer {
    share: Option<bool>,
    unreviewed_flags: Option<usize>,
}

struct Elicit<'a> {
    peer: &'a Peer<RoleServer>,
}

#[async_trait]
impl ShareConsent for Elicit<'_> {
    async fn consent(&self, preview: &SharePreview) -> Result<Consent, ShareFailure> {
        let mut schema = ElicitationSchema::builder().required_bool_property(SHARE, |field| {
            field.description("true uploads the encrypted bundle and returns its share code")
        });
        if preview.unreviewed > 0 {
            schema = schema.required_integer_property(FLAGS, |field| {
                field.description(
                    "type how many advisory flags are still unmarked, to confirm you read the report",
                )
            });
        }
        let requested_schema = schema
            .build()
            .map_err(|error| ShareFailure::Failed(error.to_owned()))?;
        let answer = self
            .peer
            .create_elicitation(ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: message(preview),
                requested_schema,
            })
            .await
            .map_err(|error| ShareFailure::Failed(format!("the confirmation failed: {error}")))?;
        if answer.action != ElicitationAction::Accept {
            return Ok(Consent::Decline);
        }
        let Some(content) = answer.content else {
            return Ok(Consent::Decline);
        };
        let answer: Answer = serde_json::from_value(content).map_err(|error| {
            ShareFailure::Failed(format!("the confirmation answer is malformed: {error}"))
        })?;
        let confirmed =
            preview.unreviewed == 0 || answer.unreviewed_flags == Some(preview.unreviewed);
        if answer.share != Some(true) || !confirmed {
            return Ok(Consent::Decline);
        }
        Ok(Consent::Share {
            unreviewed: preview.unreviewed,
        })
    }
}

fn message(preview: &SharePreview) -> String {
    let mut titles: Vec<String> = preview
        .titles
        .iter()
        .take(TITLES_SHOWN)
        .map(|title| format!("- {title}"))
        .collect();
    if preview.titles.len() > TITLES_SHOWN {
        titles.push(format!(
            "- and {} more",
            preview.titles.len() - TITLES_SHOWN
        ));
    }
    let flags = match preview.unreviewed {
        0 => String::new(),
        count => format!(
            "\n{} nobody marked yet; type the count to share anyway.",
            noun(count, "advisory flag is", "advisory flags are")
        ),
    };
    format!(
        "Share {} by {} (signed {}, {} KiB) through the RivetHub Cloud drop box?\n{}\nThe code works once and expires 7 days after upload; send it privately.{flags}",
        noun(preview.titles.len(), "session", "sessions"),
        preview.author,
        preview.signed_by,
        preview.bytes.div_ceil(1024),
        titles.join("\n"),
    )
}

pub(crate) async fn share(
    kit: &Sessions,
    args: Share,
    peer: Option<&Peer<RoleServer>>,
) -> Result<CallToolResult, Error> {
    let Some(peer) = peer.filter(|peer| {
        peer.supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
    }) else {
        return Ok(crate::refused(NO_ELICITATION.to_owned()));
    };
    let path = kit.files.readable(&args.bundle, Kind::Bundle).await?;
    match kit.share.share(path, &Elicit { peer }).await? {
        ShareOutcome::Shared { sessions, line } => Ok(crate::text(format!(
            "shared {}; {line}\nNo tool sends, posts or stores this code: give it to the user to send privately.\n",
            noun(sessions, "session", "sessions")
        ))),
        ShareOutcome::Declined => Ok(crate::refused(
            "not shared: the user did not confirm, so nothing was uploaded and the bundle stays where it is"
                .to_owned(),
        )),
    }
}
