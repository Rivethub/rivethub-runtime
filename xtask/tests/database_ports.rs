use std::process::Command;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn deny_toml_bans_a_service_from_reaching_the_database() -> Result {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = '3'\nmembers = ['*']\n",
    )?;
    let chain = ["rivethub", "gateway", "sqlx"];
    for (index, name) in chain.iter().enumerate() {
        let dependency = match chain.get(index + 1) {
            Some(next) => {
                format!("[dependencies]\n{next} = {{ path = '../{next}', version = '0.1.0' }}\n")
            }
            None => String::new(),
        };
        let member = root.join(name);
        std::fs::create_dir_all(member.join("src"))?;
        std::fs::write(member.join("src/lib.rs"), "")?;
        std::fs::write(
            member.join("Cargo.toml"),
            format!(
                "[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2024'\n{dependency}"
            ),
        )?;
    }
    std::fs::write(root.join("deny.toml"), include_str!("../../deny.toml"))?;
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["deny", "--offline", "check", "bans"])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("crate 'sqlx = 0.1.0' is explicitly banned"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn testkit_cannot_be_a_runtime_or_build_database_escape() -> Result {
    for (section, expected) in [
        ("dependencies", 1),
        ("build-dependencies", 1),
        ("dev-dependencies", 0),
        ("target.'cfg(unix)'.dependencies", 1),
        ("target.'cfg(unix)'.dev-dependencies", 0),
    ] {
        let repo = tempfile::tempdir()?;
        let root = repo.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = '3'\nmembers = ['gateway', 'testkit']\n",
        )?;
        for name in ["gateway", "testkit"] {
            let member = root.join(name);
            std::fs::create_dir_all(member.join("src"))?;
            std::fs::write(member.join("src/lib.rs"), "")?;
            let dependency = if name == "gateway" {
                format!("[{section}]\nhelper = {{ package = 'testkit', path = '../testkit' }}\n")
            } else {
                String::new()
            };
            std::fs::write(
                member.join("Cargo.toml"),
                format!(
                    "[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2024'\n{dependency}"
                ),
            )?;
        }
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .no_deps()
            .exec()?;
        let mut findings = Vec::new();
        xtask::ports::testkit_dependencies(&metadata, &mut findings);
        assert_eq!(findings.len(), expected, "{section}: {findings:?}");
    }
    Ok(())
}
