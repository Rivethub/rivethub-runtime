use std::process::Command;

use serde::Deserialize;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
struct Policy {
    bans: Bans,
}

#[derive(Deserialize)]
struct Bans {
    deny: Vec<Ban>,
}

#[derive(Deserialize)]
struct Ban {
    #[serde(rename = "crate")]
    name: String,
    wrappers: Option<Vec<String>>,
}

#[test]
fn database_wrappers_are_limited_to_authorized_crates() -> Result {
    let policy: Policy = toml::from_str(include_str!("../../deny.toml"))?;
    for database in ["sqlx", "rusqlite", "pgvector"] {
        let ban = policy
            .bans
            .deny
            .iter()
            .find(|ban| ban.name == database)
            .ok_or("database ban missing")?;
        let wrappers = ban.wrappers.as_ref().ok_or("wrappers missing")?;
        let mut expected = vec![
            "memory-postgres",
            "memory-sqlite",
            "migrate",
            "testkit",
            "rivethub",
            "cli",
        ];
        if database == "sqlx" {
            expected.push("pgvector");
        } else if database == "pgvector" {
            expected = vec!["memory-postgres"];
        } else if database == "rusqlite" {
            expected.push("capture");
        }
        assert_eq!(wrappers, &expected);
    }
    Ok(())
}

fn check_dependency(caller: &str, database: &str) -> Result<std::process::Output> {
    check_chain(&[caller, database])
}

fn check_chain(dependencies: &[&str]) -> Result<std::process::Output> {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = '3'\nmembers = ['*']\n",
    )?;
    let mut chain = Vec::new();
    if dependencies.first() != Some(&"rivethub") {
        chain.push("rivethub");
    }
    chain.extend_from_slice(dependencies);
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
    Ok(Command::new("cargo")
        .current_dir(root)
        .args(["deny", "--offline", "check", "bans"])
        .output()?)
}

#[test]
fn vector_codec_exception_is_exclusive_to_the_postgres_adapter() -> Result {
    for caller in [
        "memory-postgres",
        "gateway",
        "workers",
        "mcp",
        "capture",
        "providers",
        "memory-sqlite",
        "rivethub",
        "cli",
    ] {
        let output = check_chain(&[caller, "pgvector", "sqlx"])?;
        let stderr = String::from_utf8(output.stderr)?;
        assert_eq!(
            output.status.success(),
            caller == "memory-postgres",
            "{caller}: {stderr}"
        );
        if caller != "memory-postgres" {
            assert!(
                stderr.contains("crate 'pgvector = 0.1.0' is explicitly banned"),
                "{stderr}"
            );
        }
    }
    Ok(())
}

#[test]
fn cargo_deny_refuses_direct_database_access_from_services() -> Result {
    for service in ["gateway", "workers", "mcp", "capture"] {
        for database in ["sqlx", "rusqlite"] {
            if service == "capture" && database == "rusqlite" {
                continue;
            }
            let output = check_dependency(service, database)?;
            let stderr = String::from_utf8(output.stderr)?;
            assert!(!output.status.success(), "{service} → {database}");
            assert!(stderr.contains("error[banned]"), "{stderr}");
            assert!(
                stderr.contains(&format!("crate '{database} = 0.1.0' is explicitly banned")),
                "{stderr}"
            );
        }
    }
    Ok(())
}

#[test]
fn cargo_deny_accepts_database_access_from_adapters_and_composition() -> Result {
    for caller in [
        "memory-postgres",
        "memory-sqlite",
        "migrate",
        "testkit",
        "rivethub",
        "cli",
    ] {
        for database in ["sqlx", "rusqlite"] {
            let output = check_dependency(caller, database)?;
            assert!(
                output.status.success(),
                "{caller} → {database}: {}",
                String::from_utf8(output.stderr)?
            );
        }
    }
    let output = check_dependency("capture", "rusqlite")?;
    assert!(
        output.status.success(),
        "capture → rusqlite: {}",
        String::from_utf8(output.stderr)?
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
