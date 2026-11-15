use wire::gateway::memory::PendingTagAction;

#[test]
fn pending_tag_actions_are_written_in_lower_case() {
    for (action, json) in [
        (PendingTagAction::Add, "\"add\""),
        (PendingTagAction::Remove, "\"remove\""),
    ] {
        assert_eq!(serde_json::to_string(&action).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<PendingTagAction>(json).unwrap(),
            action
        );
    }
}
