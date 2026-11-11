use wire::gateway::roster::resume_command;
use wire::harness::HarnessId;

const NATIVE: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn each_harness_resumes_with_its_own_flag() {
    assert_eq!(
        resume_command(HarnessId::ClaudeCode, NATIVE).as_deref(),
        Some("claude --resume 00000000-0000-4000-8000-000000000001")
    );
    assert_eq!(
        resume_command(HarnessId::Codex, NATIVE).as_deref(),
        Some("codex resume 00000000-0000-4000-8000-000000000001")
    );
    assert_eq!(
        resume_command(HarnessId::KimiCode, NATIVE).as_deref(),
        Some("kimi --session 00000000-0000-4000-8000-000000000001")
    );
}

#[test]
fn a_harness_without_a_resume_flag_has_no_command() {
    assert_eq!(resume_command(HarnessId::Dsh, NATIVE), None);
    assert_eq!(resume_command(HarnessId::Cowork, NATIVE), None);
}
