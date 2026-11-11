use clap::CommandFactory as _;

fn documented() -> std::io::Result<String> {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/CLI.md"))
}

fn visible_leaves(command: &clap::Command, prefix: &str, leaves: &mut Vec<String>) {
    for sub in command.get_subcommands() {
        if sub.is_hide_set() || sub.get_name() == "help" {
            continue;
        }
        let path = format!("{prefix} {}", sub.get_name());
        let before = leaves.len();
        visible_leaves(sub, &path, leaves);
        if leaves.len() == before {
            leaves.push(path);
        }
    }
}

#[test]
fn every_visible_command_is_in_the_operator_cli_doc() -> std::io::Result<()> {
    let doc = documented()?;
    let mut leaves = Vec::new();
    visible_leaves(&cli::Cli::command(), "rh", &mut leaves);
    assert!(leaves.contains(&"rh harness connect".to_owned()));
    let missing: Vec<&String> = leaves
        .iter()
        .filter(|path| !doc.contains(&format!("`{path}`")))
        .collect();
    assert_eq!(missing, Vec::<&String>::new());
    Ok(())
}
