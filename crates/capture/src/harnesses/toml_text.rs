pub(super) fn append_block(text: &str, block: &str) -> String {
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(block);
    out
}

fn header_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let inner = trimmed
        .strip_prefix("[[")
        .or_else(|| trimmed.strip_prefix('['))?;
    let (name, _) = inner.split_once(']')?;
    Some(
        name.chars()
            .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t'))
            .collect(),
    )
}

pub(super) fn remove_tables(text: &str, parent: &str, name: &str) -> String {
    let own = format!("{parent}.{name}");
    let nested = format!("{own}.");
    let mut out = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        if let Some(header) = header_name(line) {
            skipping = header == own || header.starts_with(&nested);
        }
        if !skipping {
            out.push_str(line);
        }
    }
    if skipping {
        while out.ends_with("\n\n") {
            out.pop();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_appended_then_removed_restores_the_text() {
        let original = "model = \"x\"\n\n[mcp_servers.other]\ncommand = \"a\"\n";
        let block = "[mcp_servers.rivethub]\ncommand = \"/bin/rh\"\nargs = [\"mcp\"]\n";
        let added = append_block(original, block);
        assert!(added.ends_with(block));
        assert_eq!(remove_tables(&added, "mcp_servers", "rivethub"), original);
    }

    #[test]
    fn nested_tables_of_the_named_server_go_with_it() {
        let text = "[mcp_servers.rivethub]\ncommand = \"a\"\n[mcp_servers.rivethub.env]\nK = \"v\"\n[other]\nx = 1\n";
        assert_eq!(
            remove_tables(text, "mcp_servers", "rivethub"),
            "[other]\nx = 1\n"
        );
    }
}
