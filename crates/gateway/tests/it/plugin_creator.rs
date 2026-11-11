use std::path::Path;

use axum::http::{Method, StatusCode};
use serde::Serialize;
use wire::JsonObject;
use wire::plugin::RouteName;
use wire::plugin::host::{PluginState, PluginToolList};
use wire::plugin::manifest::Permission;
use wire::plugin::process::{PluginToolCall, PluginToolResult};

use crate::plugin_support::{
    Failure, Harness, TestResult, enable, harness, write_manifest, write_manifest_with,
};

const BASE: [Permission; 2] = [Permission::SessionsRead, Permission::TopicsRead];

#[derive(Clone, Copy, Serialize)]
struct Scaffold<'a> {
    template: &'a str,
    id: &'a str,
    name: &'a str,
    description: &'a str,
}

#[derive(Serialize)]
struct Named<'a> {
    id: &'a str,
}

#[derive(Serialize)]
struct Nothing {}

struct Creator {
    client: reqwest::Client,
    token: String,
}

impl Creator {
    fn open(root: &Path) -> Result<Self, Failure> {
        let folder = root.join("state/plugins");
        Ok(Self {
            client: reqwest::Client::builder()
                .unix_socket(folder.join("tools.sock"))
                .build()?,
            token: std::fs::read_to_string(folder.join("tools.token"))?
                .trim()
                .to_owned(),
        })
    }

    async fn names(&self) -> Result<Vec<String>, Failure> {
        let list: PluginToolList = self
            .client
            .get("http://plugins/tools")
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;
        Ok(list
            .tools
            .iter()
            .map(|entry| kernel::plugins::tool_name(&entry.plugin, &entry.tool.name))
            .collect())
    }

    async fn call(
        &self,
        tool: &str,
        arguments: &impl Serialize,
    ) -> Result<PluginToolResult, Failure> {
        let call = PluginToolCall {
            arguments: JsonObject::from_serialize(arguments)?,
            session_id: None,
        };
        Ok(self
            .client
            .post(format!("http://plugins/tools/plugins-{tool}"))
            .bearer_auth(&self.token)
            .json(&call)
            .send()
            .await?
            .json()
            .await?)
    }

    async fn install(&self, id: &str) -> Result<PluginToolResult, Failure> {
        self.call("install", &Named { id }).await
    }
}

fn page_view(folder: &Path, permissions: &[Permission], view: &str) -> TestResult {
    let view = RouteName::try_from(view.to_owned())?;
    write_manifest_with(folder, permissions, |manifest| {
        if let Some(page) = manifest.contributes.pages.first_mut() {
            page.view = view;
        }
    })
}

fn says(answer: &PluginToolResult, text: &str) {
    assert!(answer.text.contains(text), "{}", answer.text);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_creator_builds_and_reloads_but_only_a_paired_device_turns_a_plugin_on() -> TestResult {
    let root = tempfile::tempdir()?;
    let host = harness(root.path()).await?;
    let creator = Creator::open(root.path())?;
    let names = creator.names().await?;
    for tool in [
        "plugins-templates",
        "plugins-scaffold",
        "plugins-install",
        "plugins-status",
    ] {
        assert!(names.contains(&tool.to_owned()), "{names:?}");
    }
    let workshop = root.path().join("state/workshop");
    let folder = workshop.join("fixture");
    says(
        &creator.call("templates", &Nothing {}).await?,
        "no plug-in templates",
    );
    let template = root.path().join("state/plugin-templates/example");
    write_manifest(&template, &BASE)?;
    std::fs::write(template.join("GUIDE.md"), "How to build it.")?;
    says(
        &creator.call("templates", &Nothing {}).await?,
        "- example: Fixture 0.1.0.",
    );
    scaffold(&host, &creator, &folder).await?;
    says(&creator.install("fixture").await?, "Approve and turn on");
    let record = host.record().await?;
    assert!(!record.enabled && record.granted.is_empty());
    turn_on(&host, &record.pending).await?;
    reload(&host, &creator, &folder).await?;
    std::os::unix::fs::symlink(&folder, workshop.join("linked"))?;
    let linked = creator.install("linked").await?;
    assert!(linked.is_error);
    says(&linked, "not a folder");
    host.gateway.close().await;
    Ok(())
}

async fn scaffold(host: &Harness, creator: &Creator, folder: &Path) -> TestResult {
    let asked = Scaffold {
        template: "example",
        id: "fixture",
        name: "Fixture",
        description: "A fixture.",
    };
    let made = creator.call("scaffold", &asked).await?;
    says(&made, "GUIDE.md");
    for file in ["rivethub-plugin.json", "fixture-plugin", "GUIDE.md"] {
        assert!(folder.join(file).is_file(), "{file}");
    }
    assert!(creator.call("scaffold", &asked).await?.is_error);
    let reserved = Scaffold {
        id: "plugins",
        ..asked
    };
    says(&creator.call("scaffold", &reserved).await?, "reserved");
    let proxied = Some(r#"{"arguments": {}}"#.to_owned());
    let path = "/api/plugins/plugins/tools/status";
    let (status, _) = host.call(Method::POST, path, proxied, false).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

async fn reload(host: &Harness, creator: &Creator, folder: &Path) -> TestResult {
    let widened = [BASE[0], BASE[1], Permission::NotificationsSend];
    page_view(folder, &widened, "status")?;
    says(&creator.install("fixture").await?, "notifications.send");
    let record = host.record().await?;
    assert_eq!(record.state, PluginState::AwaitingApproval);
    assert_eq!(record.granted, BASE.to_vec());
    page_view(folder, &BASE, "broken")?;
    let reloaded = creator.install("fixture").await?;
    says(&reloaded, "It is running.");
    says(&reloaded, "Page Fixture (view broken): ");
    let status = creator.call("status", &Named { id: "fixture" }).await?;
    says(&status, "refused the http://plugin/views/broken answer");
    Ok(())
}

async fn turn_on(host: &Harness, pending: &[Permission]) -> TestResult {
    let (status, bytes) = host
        .call(
            Method::POST,
            "/api/plugins/fixture/enable",
            enable(pending)?,
            true,
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    host.until("running", |record| record.state == PluginState::Running)
        .await?;
    Ok(())
}
