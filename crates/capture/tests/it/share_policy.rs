use capture::seal::ShareServer;
use capture::share_policy::SharePolicy;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const POLICY: &str = "disabled by this hub's policy";
const LIST_KEY: &str = "memory.share.allowed_servers";

fn policy(cloud_disabled: bool, allowed: &[&str]) -> Result<SharePolicy, Box<dyn std::error::Error>> {
    let mut yaml = String::from("memory:\n  backend: sqlite\n");
    if !allowed.is_empty() {
        yaml.push_str("  share:\n    allowed_servers:\n");
        for host in allowed {
            yaml.push_str(&format!("      - {host}\n"));
        }
    }
    if cloud_disabled {
        yaml.push_str("cloud:\n  disabled: true\n");
    }
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml)?;
    Ok(SharePolicy::from_config(&config::type_document(&document)?.config)?)
}

fn host(text: &str) -> Result<ShareServer, Box<dyn std::error::Error>> {
    Ok(text.parse()?)
}

fn refusal(result: Result<impl std::fmt::Debug, impl std::fmt::Display>) -> String {
    match result {
        Ok(value) => format!("admitted {value:?}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn an_open_hub_with_no_list_admits_any_host() -> TestResult {
    let open = policy(false, &[])?;
    assert!(open.admit(&host("team.example.com")?).is_ok());
    assert!(open.admit(&ShareServer::rivethub_cloud()).is_ok());
    assert_eq!(open.default_server(None)?, ShareServer::rivethub_cloud());
    Ok(())
}

#[test]
fn a_list_admits_only_its_hosts() -> TestResult {
    let listed = policy(false, &["https://Team.Example.com/", "drops.example.com:8443"])?;
    assert!(listed.admit(&host("team.example.com")?).is_ok());
    assert!(listed.admit(&host("drops.example.com:8443")?).is_ok());
    for outside in ["other.example.com", "drops.example.com", "rivethub.cloud"] {
        let refused = refusal(listed.admit(&host(outside)?));
        assert!(refused.contains(POLICY), "{outside}: {refused}");
        assert!(refused.contains(LIST_KEY), "{outside}: {refused}");
    }
    let refused = refusal(listed.default_server(None));
    assert!(refused.contains(POLICY), "{refused}");
    assert!(policy(false, &["rivethub.cloud"])?.default_server(None).is_ok());
    Ok(())
}

#[test]
fn a_disabled_cloud_refuses_rivethub_cloud_even_when_listed() -> TestResult {
    let closed = policy(true, &["rivethub.cloud", "team.example.com"])?;
    let refused = refusal(closed.admit(&ShareServer::rivethub_cloud()));
    assert!(refused.contains(POLICY), "{refused}");
    assert!(refused.contains("cloud.disabled"), "{refused}");
    assert!(closed.admit(&host("team.example.com")?).is_ok());
    let refused = refusal(closed.default_server(None));
    assert!(refused.contains(POLICY), "{refused}");
    assert_eq!(
        closed.default_server(Some(host("team.example.com")?))?,
        host("team.example.com")?
    );
    Ok(())
}

#[test]
fn a_disabled_cloud_with_no_list_turns_sharing_off() -> TestResult {
    let off = policy(true, &[])?;
    for refused in [
        refusal(off.admit(&host("team.example.com")?)),
        refusal(off.admit(&ShareServer::rivethub_cloud())),
        refusal(off.default_server(None)),
        refusal(off.default_server(Some(host("team.example.com")?))),
    ] {
        assert!(refused.contains(POLICY), "{refused}");
        assert!(refused.contains(LIST_KEY), "{refused}");
    }
    Ok(())
}

#[test]
fn a_list_entry_that_is_not_a_host_is_a_config_error() -> TestResult {
    let error = match policy(false, &["team.example.com", "ftp://team.example.com"]) {
        Ok(_) => return Err("a bad entry was accepted".into()),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(LIST_KEY), "{error}");
    assert!(error.contains("ftp://team.example.com"), "{error}");
    Ok(())
}
