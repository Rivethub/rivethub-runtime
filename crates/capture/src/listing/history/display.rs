const THINKING_CHARS: usize = 8000;

pub(super) fn tail_chars(thinking: &str) -> String {
    let count = thinking.chars().count();
    if count <= THINKING_CHARS {
        return thinking.to_owned();
    }
    let mut kept = String::from("…");
    kept.extend(thinking.chars().skip(count - THINKING_CHARS));
    kept
}

pub(super) fn reply_after(text: &str, spoken: usize) -> Option<String> {
    let reply = text.get(spoken..)?.trim();
    (spoken > 0 && !reply.is_empty()).then(|| reply.to_owned())
}

pub(super) fn token_label(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = (tokens + 50_000) / 100_000;
        format!("{}.{}M tokens", tenths / 10, tenths % 10)
    } else if tokens >= 1000 {
        format!("{}k tokens", (tokens + 500) / 1000)
    } else {
        format!("{tokens} tokens")
    }
}
