use kernel::memory::session_title;

const NATIVE: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn a_real_title_is_kept() {
    assert_eq!(
        session_title(Some("Fix the deploy"), Some("summary"), Some("hi"), NATIVE),
        "Fix the deploy"
    );
}

#[test]
fn an_untitled_session_takes_the_first_line_of_its_summary() {
    assert_eq!(
        session_title(
            None,
            Some("\nReleased 0.8.\nMore detail"),
            Some("hi"),
            NATIVE
        ),
        "Released 0.8."
    );
}

#[test]
fn a_placeholder_title_falls_back_to_the_first_user_message_with_paths_shortened() {
    let prompt = "Read /home/user/.claude/skills/poteto-mode/SKILL.md in full first, then act";
    let title = session_title(Some(NATIVE), None, Some(prompt), NATIVE);
    assert_eq!(title, "Read SKILL.md in full first, then act");
    assert!(!title.contains('/'));
}

#[test]
fn a_long_message_is_cut_to_eighty_characters() {
    let prompt = "word ".repeat(60);
    let title = session_title(None, None, Some(&prompt), NATIVE);
    assert!(title.chars().count() <= 81);
    assert!(title.ends_with('…'));
}

#[test]
fn a_bare_path_never_becomes_a_title() {
    assert_eq!(
        session_title(None, None, Some("/home/user/work/"), NATIVE),
        "work"
    );
    assert_eq!(session_title(None, None, Some("   "), NATIVE), NATIVE);
}

#[test]
fn the_fallback_title_is_what_the_person_typed_after_injected_blocks() {
    for prompt in [
        "# AGENTS.md instructions for /home/user/app\n\n<INSTRUCTIONS>\nkeep it short\n</INSTRUCTIONS>\nfix the flaky cache test",
        "<user_instructions>be brief</user_instructions>\n<environment_context>\n  <cwd>/home/user/app</cwd>\n</environment_context>\nfix the flaky cache test",
        "<system-reminder>\nthe date is today\n</system-reminder>\nfix the flaky cache test",
    ] {
        assert_eq!(
            session_title(None, None, Some(prompt), NATIVE),
            "fix the flaky cache test",
            "{prompt}"
        );
    }
    assert_eq!(
        session_title(
            None,
            None,
            Some("# AGENTS.md instructions for /home/user/app"),
            NATIVE
        ),
        NATIVE,
        "nothing typed yet leaves the native id"
    );
}
