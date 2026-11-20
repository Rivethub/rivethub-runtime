#[path = "llm/top_down.rs"]
pub(crate) mod llm;
#[path = "plan.rs"]
pub(crate) mod plan;
#[path = "prompt/top_down.rs"]
pub(crate) mod prompt;
#[path = "settings/top_down.rs"]
pub(crate) mod settings;

fn python_whitespace(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}')
}
