use chrono::DateTime;
use chrono::Utc;
use kernel::transcript::StoreSession;
use kernel::transcript::linked_parent;
use wire::harness::HarnessSessionStatus;
use wire::harness::HarnessSessionSummary;
use wire::session_id::SessionId;

pub(crate) fn summary(
    row: &StoreSession,
    cwd: Option<String>,
    now: DateTime<Utc>,
) -> HarnessSessionSummary {
    let updated_at = if row.updated_at == 0 {
        now
    } else {
        epoch_ms(row.updated_at)
    };
    HarnessSessionSummary {
        session_id: SessionId::new(row.harness, &row.id),
        harness_id: row.harness,
        title: (row.title != row.id).then(|| row.title.clone()),
        cwd,
        created_at: epoch_ms(row.created_at.unwrap_or(row.updated_at)),
        updated_at,
        status: HarnessSessionStatus::Ended,
        blocked: None,
        supersedes: None,
        model: row.model.clone(),
        parent_session_id: row
            .parent_session_id
            .as_deref()
            .and_then(|parent| linked_parent(row.harness, parent)),
        agent_name: row.agent_name.clone(),
        task_id: row.task_id.clone(),
        effort: None,
        transport: None,
        extra: wire::Extra::default(),
    }
}

fn epoch_ms(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::Utc;
    use kernel::transcript::StoreSession;
    use wire::harness::HarnessId;
    use wire::harness::HarnessSessionStatus;
    use wire::session_id::SessionId;

    use super::summary;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap_or_default()
    }

    #[test]
    fn summary_of_an_untitled_zero_time_row() {
        let row = StoreSession::new(HarnessId::ClaudeCode, "u", 0);
        let summary = summary(&row, None, now());
        assert_eq!(
            summary.session_id,
            SessionId::new(HarnessId::ClaudeCode, "u")
        );
        assert_eq!(summary.session_id.as_str(), "claude-code:u");
        assert_eq!(summary.harness_id, HarnessId::ClaudeCode);
        assert_eq!(summary.title, None);
        assert_eq!(summary.status, HarnessSessionStatus::Ended);
        assert_eq!(summary.created_at, DateTime::UNIX_EPOCH);
        assert_eq!(summary.updated_at, now());
        let json = serde_json::to_string(&summary).unwrap_or_default();
        assert!(
            json.contains(r#""createdAt":"1970-01-01T00:00:00.000Z""#),
            "{json}"
        );
        assert!(json.contains(r#""status":"ended""#), "{json}");
        assert!(!json.contains("title"), "{json}");
    }

    #[test]
    fn summary_copies_labels_and_dates() {
        let mut row = StoreSession::new(HarnessId::GrokBuild, "g", 1_790_812_800_000);
        row.title = "plan".to_owned();
        row.model = Some("grok-4".to_owned());
        row.agent_name = Some("explore".to_owned());
        row.parent_session_id = Some("p".to_owned());
        let summary = summary(&row, Some("/home/user/w".to_owned()), now());
        assert_eq!(summary.title.as_deref(), Some("plan"));
        assert_eq!(summary.cwd.as_deref(), Some("/home/user/w"));
        assert_eq!(summary.created_at, summary.updated_at);
        assert_eq!(summary.updated_at.to_rfc3339(), "2026-10-01T00:00:00+00:00");
        assert_eq!(summary.model.as_deref(), Some("grok-4"));
        assert_eq!(summary.agent_name.as_deref(), Some("explore"));
        assert_eq!(
            summary.parent_session_id,
            Some(SessionId::new(HarnessId::GrokBuild, "p"))
        );
    }

    #[test]
    fn summary_drops_parent_links_that_cannot_be_session_ids() {
        for parent in [" p", "p ", "a/b", "..", ""] {
            let mut row = StoreSession::new(HarnessId::ClaudeCode, "c", 1);
            row.parent_session_id = Some(parent.to_owned());
            assert_eq!(
                summary(&row, None, now()).parent_session_id,
                None,
                "{parent:?}"
            );
        }
    }
}
