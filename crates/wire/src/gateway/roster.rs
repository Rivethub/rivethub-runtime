use crate::harness::HarnessId;

#[must_use]
pub fn resume_command(harness: HarnessId, native: &str) -> Option<String> {
    let (program, resume) = match harness {
        HarnessId::ClaudeCode => ("claude", "--resume"),
        HarnessId::GrokBuild => ("grok", "--resume"),
        HarnessId::Hermes => ("hermes", "--resume"),
        HarnessId::Cursor => ("agent", "--resume"),
        HarnessId::Omp => ("omp", "--resume"),
        HarnessId::KimiCode => ("kimi", "--session"),
        HarnessId::Opencode => ("opencode", "--session"),
        HarnessId::Codex => ("codex", "resume"),
        HarnessId::Dsh | HarnessId::Cowork => return None,
    };
    Some(format!("{program} {resume} {native}"))
}
