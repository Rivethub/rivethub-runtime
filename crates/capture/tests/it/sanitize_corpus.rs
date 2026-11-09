use capture::sanitize;
use capture::sanitize::Policy;
use chrono::DateTime;
use chrono::Utc;
use serde_json::json;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy)]
enum Position {
    Content,
    ToolResult,
    ToolArgsValue,
    ToolArgsKey,
    ToolArgsJson,
    MetadataValue,
    MetadataKey,
    Title,
    SettingsValue,
    SessionKey,
    Channel,
    TaskId,
    ToolName,
    Agent,
    OriginNode,
    EventId,
}

struct Shape {
    name: &'static str,
    carrier: &'static str,
    planted: &'static [&'static str],
}

struct Case {
    name: &'static str,
    position: Position,
    carrier: &'static str,
    planted: &'static [&'static str],
}

macro_rules! shapes {
    ($($name:literal => $carrier:expr, [$($planted:expr),+];)+) => {
        &[$(Shape { name: $name, carrier: $carrier, planted: &[$($planted),+] }),+]
    };
}

macro_rules! cases {
    ($($name:literal => $position:expr, $carrier:expr, [$($planted:expr),+];)+) => {
        &[$(Case { name: $name, position: $position, carrier: $carrier, planted: &[$($planted),+] }),+]
    };
}

macro_rules! position_tests {
    ($($test:ident => $position:expr;)+) => {
        $(
            #[test]
            fn $test() -> TestResult {
                no_shape_survives($position)
            }
        )+
    };
}

const SHAPES: &[Shape] = shapes! {
    "openai_sk" => "key is {0} ok", [concat!("sk-", "test-PLANTED8RmT2vLx9WcN4bKp7HyJ3dFs")];
    "openai_proj" => "using {0}", [concat!("sk-", "proj-PLANTEDvLx9WcN4bKp7HyJ3dFs6GtAe_vLx9WcN4")];
    "anthropic" => "ANTHROPIC key {0}", [concat!("sk-", "ant-api03-PLANTEDcN4bKp7HyJ3dFs6GtAeUo5Vq8RmT2vLx9WcN4bKp7HyJ3dFs6GtAeUo5Vq8RAA")];
    "stripe_sk_live" => "stripe {0}", [concat!("sk_live_", "PLANTEDp7HyJ3dFs6GtAeUo5Vq8")];
    "stripe_rk_live" => "stripe restricted {0}", [concat!("rk_live_", "PLANTED3dFs6GtAeUo5Vq8RmT2v")];
    "google_api_key" => "maps key {0}", [concat!("AIza", "SyPLANTEDGtAeUo5Vq8RmT2vLx9WcN4bKp7")];
    "gitlab_pat" => "gitlab {0}", [concat!("glpat-", "PLANTEDo5Vq8RmT2vLx9")];
    "npm_token" => "npm {0}", [concat!("npm_", "PLANTEDRmT2vLx9WcN4bKp7HyJ3dFs6GtAeU")];
    "huggingface" => "hf {0}", [concat!("hf_", "PLANTEDLx9WcN4bKp7HyJ3dFs6GtAeUo5V")];
    "sendgrid" => "mail {0}", [concat!("SG.", "PLANTEDN4bKp7HyJ3dFs6G.PLANTEDN4bKp7HyJ3dFs6GtAeUo5Vq8RmT2vLx9WcN4")];
    "bearer_header" => "Authorization: Bearer {0}", ["PLANTEDbearer7HyJ3dFs6GtAeUo5Vq8RmT"];
    "bearer_lower" => "curl -H 'authorization: bearer {0}'", ["PLANTEDbearerlcdFs6GtAeUo5Vq8RmT2vLx9"];
    "jwt_bare" => "token was {0}", [concat!("eyJ", "hbGciOiJIUzI1NiJ9.eyJ", "zdWIiOiJQTEFOVEVEIn0.PLANTEDjwtsigtAeUo5Vq8RmT2vLx9WcN4bKp"), "PLANTEDjwtsigtAeUo5Vq8RmT2vLx9WcN4bKp"];
    "basic_auth" => "Authorization: Basic {0}", ["dXNlcjpQTEFOVEVEYmFzaWM1VnE4Um1UMnZM"];
    "pem_rsa" => concat!("-----BEGIN ", "RSA PRIVATE KEY-----\n{0}\n{1}\n-----END ", "RSA PRIVATE KEY-----"), ["MIIEpPLANTEDpemRSAmT2vLx9WcN4bKp7HyJ3dFs6GtAeU", "PLANTEDpemline2RSAmT2vLx9WcN4bKp7HyJ3dFs6GtAeU"];
    "pem_pkcs8" => concat!("-----BEGIN ", "PRIVATE KEY-----\n{0}\n{1}\n-----END ", "PRIVATE KEY-----"), ["MIIEvPLANTEDpemPKCS8x9WcN4bKp7HyJ3dFs6GtAeUo5Vq8", "PLANTEDpemline2PKCS8x9WcN4bKp7HyJ3dFs6GtAeUo5Vq8"];
    "pem_openssh" => concat!("-----BEGIN ", "OPENSSH PRIVATE KEY-----\n{0}\n{1}\n-----END ", "OPENSSH PRIVATE KEY-----"), ["MIIEpPLANTEDpemOPENSSH4bKp7HyJ3dFs6GtAeUo5Vq8RmT2v", "PLANTEDpemline2OPENSSH4bKp7HyJ3dFs6GtAeUo5Vq8RmT2v"];
    "pem_ec" => concat!("-----BEGIN ", "EC PRIVATE KEY-----\n{0}\n{1}\n-----END ", "EC PRIVATE KEY-----"), ["MIIEpPLANTEDpemECHyJ3dFs6GtAeUo5Vq8RmT2vLx9Wc", "PLANTEDpemline2ECHyJ3dFs6GtAeUo5Vq8RmT2vLx9Wc"];
    "pem_encrypted" => concat!("-----BEGIN ", "ENCRYPTED PRIVATE KEY-----\n{0}\n{1}\n-----END ", "ENCRYPTED PRIVATE KEY-----"), ["MIIEpPLANTEDpemENCRYPTEDFs6GtAeUo5Vq8RmT2vLx9WcN4bKp", "PLANTEDpemline2ENCRYPTEDFs6GtAeUo5Vq8RmT2vLx9WcN4bKp"];
    "pem_crlf" => concat!("-----BEGIN ", "RSA PRIVATE KEY-----\r\n{0}\r\n{1}\r\n-----END ", "RSA PRIVATE KEY-----"), ["MIIEpPLANTEDpemRSAAeUo5Vq8RmT2vLx9WcN4bKp7HyJ3", "PLANTEDpemline2RSAAeUo5Vq8RmT2vLx9WcN4bKp7HyJ3"];
    "pem_gcp_json_escaped" => concat!("{\"type\": \"service_account\", \"private_key\": \"-----BEGIN ", "PRIVATE KEY-----\\n{0}\\n-----END ", "PRIVATE KEY-----\\n\"}"), ["MIIEvPLANTEDgcpVq8RmT2vLx9WcN4bKp7HyJ3dFs6GtA"];
    "pem_pgp" => concat!("-----BEGIN ", "PGP PRIVATE KEY BLOCK-----\n\n{0}\n-----END ", "PGP PRIVATE KEY BLOCK-----"), ["lQPGBPLANTEDpgpT2vLx9WcN4bKp7HyJ3dFs6GtAe"];
    "pem_truncated" => concat!("$ head -2 id_rsa\n-----BEGIN ", "RSA PRIVATE KEY-----\n{0}"), ["MIIEpPLANTEDtrunc9WcN4bKp7HyJ3dFs6GtAeUo5Vq"];
    "postgres_url" => "postgres://app:{0}@db.example.test:5432/app", ["PLANTEDpgbKp7HyJ3dF"];
    "postgresql_url" => "DATABASE_URL=postgresql://app:{0}@db.example.test/app", ["PLANTEDpgqlyJ3dFs6GtA"];
    "mysql_url" => "mysql://root:{0}@127.0.0.1:3306/db", ["PLANTEDmys6GtAeUo5V"];
    "mongodb_srv" => "mongodb+srv://admin:{0}@cluster0.example.mongodb.net/test", ["PLANTEDmongoeUo5Vq8RmT"];
    "redis_user" => "redis://default:{0}@cache.example.test:6379/0", ["PLANTEDredisdefq8RmT2vLx9"];
    "redis_empty_user" => "redis://:{0}@cache.example.test:6379/0", ["PLANTEDredis2vLx9WcN4b"];
    "https_url_password" => "git clone https://deploy:{0}@git.example.test/repo.git", ["PLANTEDhttpWcN4bKp7Hy"];
    "url_pw_pct_encoded" => "postgres://u:{0}@h.example.test/db", ["PLANTED%2FpctKp7HyJ3dFs"];
    "url_pw_raw_slash" => "postgres://u:{0}@h.example.test/db", ["PLANTEDsl/ashJ3dFs6GtAe", "ashJ3dFs6GtAe@"];
    "url_pw_raw_at" => "postgres://u:{0}@{1}@h.example.test/db", ["PLANTEDatA6GtAeUo5Vq", "PLANTEDatB6GtAeUo5Vq"];
    "ado_connstr" => "Server=db;User Id=sa;Password={0};", ["PLANTEDadoUo5Vq8RmT2"];
    "jdbc_query_pw" => "jdbc:postgresql://db:5432/app?user=app&password={0}", ["PLANTEDjdbc8RmT2vLx9W"];
    "libpq_kv" => "host=db user=app password={0} dbname=app", ["PLANTEDlibpqvLx9WcN4bK"];
    "azure_accountkey" => "DefaultEndpointsProtocol=https;AccountName=x;AccountKey={0};", ["PLANTEDazurecN4bKp7HyJ3dFs6GtAeUo5Vq=="];
    "env_password" => "DATABASE_PASSWORD={0}", ["PLANTEDenvpwp7HyJ3dFs6"];
    "env_export_dq" => "export OPENAI_API_KEY=\"{0}\"", ["PLANTEDenvdq3dFs6GtAeU"];
    "env_sq" => "STRIPE_SECRET_KEY='{0}'", ["PLANTEDenvsqGtAeUo5Vq8"];
    "env_multiline_file" => "# .env\nPORT=3000\nJWT_SECRET={0}\nGITHUB_TOKEN={1}\n", ["PLANTEDenvf1o5Vq8RmT2v", "PLANTEDenvf2o5Vq8RmT2v"];
    "env_db_pass" => "DB_PASS={0}", ["PLANTEDdbpassRmT2vLx9Wc"];
    "env_app_key" => "APP_KEY=base64:{0}", ["PLANTEDappkeyLx9WcN4bKp"];
    "pgpassword_env" => "PGPASSWORD={0} psql -h db", ["PLANTEDpgpwN4bKp7HyJ3"];
    "yaml_password" => "database:\n  user: app\n  password: {0}\n", ["PLANTEDyaml7HyJ3dFs6G"];
    "toml_db_password" => "db_password = \"{0}\"", ["PLANTEDtomldFs6GtAeUo"];
    "assign_api_key" => "api_key: {0}", ["PLANTEDassigntAeUo5Vq8R"];
    "x_api_key_header" => "X-Api-Key: {0}", ["PLANTEDxapi5Vq8RmT2vL"];
    "query_access_token" => "https://api.example.test/v1?access_token={0}", ["PLANTEDqatmT2vLx9WcN"];
    "client_secret" => "client_secret={0}", ["PLANTEDcsx9WcN4bKp7"];
    "aws_akia" => "aws key {0}", [concat!("AKIA", "PLANTED4BKP7HYJ3")];
    "aws_asia" => "sts key {0}", [concat!("ASIA", "PLANTEDHYJ3DFS6G")];
    "aws_secret_ini" => "[default]\naws_secret_access_key = {0}\n", ["PLANTEDawssecFs6GtAeUo5Vq/Fs6GtAeUo5Vq8RmT"];
    "aws_secret_env" => "AWS_SECRET_ACCESS_KEY={0}", ["PLANTEDawsenvAeUo5Vq8RmT2/AeUo5Vq8"];
    "gh_ghp" => "gh {0}", [concat!("ghp_", "PLANTEDVq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt")];
    "gh_gho" => "gh {0}", [concat!("gho_", "PLANTEDT2vLx9WcN4bKp7HyJ3dFs6GtAeUo5")];
    "gh_ghs" => "gh {0}", [concat!("ghs_", "PLANTED9WcN4bKp7HyJ3dFs6GtAeUo5Vq8Rm")];
    "gh_fine_grained" => "gh {0}", [concat!("github_pat_", "11PLANTEDbKp7HyJ3dFs6G_PLANTEDbKp7HyJ3dFs6GtAeUo5Vq8RmT2vLx9WcN4bKp7HyJ3dFs6GtAeUo")];
    "slack_xoxb" => "slack {0}", [concat!("xoxb-", "8123456790-9042187365-PLANTEDyJ3dFs6GtAeUo5Vq8")];
    "slack_xoxp" => "slack {0}", [concat!("xoxp-", "8123456790-9042187365-7731908265-PLANTEDs6GtAeUo5Vq8RmT2vLx9WcN4b")];
    "slack_xapp" => "slack {0}", [concat!("xapp-", "1-A0123456789-8123456790123-PLANTEDeUo5Vq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt")];
    "slack_webhook" => "https://hooks.slack.com/services/T0A1B2C3D/B0E1F2G3H/{0}", ["PLANTEDhookq8RmT2vLx9WcN"];
    "cookie_header" => "Cookie: sessionid={0}", ["PLANTEDcookie2vLx9WcN4b"];
    "curl_u" => "curl -u admin:{0} https://h.example.test", ["PLANTEDcurluWcN4bKp7Hy"];
    "mysql_p_flag" => "mysql -u root -p{0} app", ["PLANTEDmypKp7HyJ3dFs"];
};

const CASES: &[Case] = cases! {
    "env_midline" => Position::ToolArgsJson, r#"{"command": "cd /srv && AWS_SECRET_ACCESS_KEY={0} aws s3 ls"}"#, ["PLANTEDawsZq8RmT2vLx9W"];
    "export_after_and" => Position::Content, "cd /app && export DB_PASSWORD={0} && ./run", ["PLANTEDchainZq8RmT2vLx9W"];
    "docker_env_json" => Position::ToolResult, r#"[{"Config":{"Env":["POSTGRES_PASSWORD={0}","PATH=/usr/bin"]}}]"#, ["PLANTEDdockZq8RmT2vLx9W"];
    "yaml_password" => Position::Content, "db:\n  user: app\n  password: {0}\n", ["PLANTEDyamlZq8RmT2vLx9W"];
    "json_password_key_in_text" => Position::Content, r#"{"user":"app","password":"{0}"}"#, ["PLANTEDjtextZq8RmT2vLx9W"];
    "escaped_json_apikey" => Position::ToolResult, r#""{\"api_key\": \"{0}\"}""#, ["PLANTEDescZq8RmT2vLx9W"];
    "nested_under_secret_key" => Position::ToolArgsJson, r#"{"auth": {"value": "{0}"}}"#, ["PLANTEDnestZq8RmT2vLx9W"];
    "numeric_password" => Position::ToolArgsJson, r#"{"password": 98127341987234}"#, ["98127341987234"];
    "url_pw_with_at" => Position::Content, "postgres://app:{0}@x@db.example.com/app", ["PLANTEDatZq8RmT2vLx9W"];
    "url_pw_with_slash" => Position::Content, "postgres://app:{0}/x@db.example.com/app", ["PLANTEDslZq8RmT2vLx9W"];
    "placeholder_adjacent" => Position::Content, "api_key = [REDACTED]{0}", ["PLANTEDadjZq8RmT2vLx9W"];
    "ghp_in_placeholder" => Position::Content, "token [REDACTED:{0}]", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt0")];
    "ghp_in_session_key" => Position::SessionKey, "{0}", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt1")];
    "ghp_in_event_id" => Position::EventId, "{0}", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt2")];
    "ghp_in_task_id" => Position::TaskId, "{0}", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt3")];
    "ghp_in_tool_name" => Position::ToolName, "mcp__x__{0}", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt4")];
    "ghp_in_channel" => Position::Channel, "{0}", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt5")];
    "ghp_in_node_name" => Position::OriginNode, "{0}", [concat!("ghp_", "PLANTEDq8RmT2vLx9WcN4bKp7HyJ3dFs6Gt6")];
    "lowercase_sk_control" => Position::Content, "key {0}", [concat!("sk-", "test-PLANTED-q8RmT2vLx9WcN4bKp7HyJ")];
};

fn at() -> Result<DateTime<Utc>, chrono::ParseError> {
    Ok(DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")?.with_timezone(&Utc))
}

fn object(text: &str) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_str(text)
}

fn object_of(value: &serde_json::Value) -> Result<JsonObject, serde_json::Error> {
    serde_json::from_value(value.clone())
}

fn fill(template: &str, planted: &[&str]) -> String {
    planted
        .iter()
        .enumerate()
        .fold(template.to_owned(), |text, (index, value)| {
            text.replace(&format!("{{{index}}}"), value)
        })
}

fn bundle_with(
    position: Position,
    text: &str,
) -> Result<SessionBundle, Box<dyn std::error::Error>> {
    let mut node = "node-1".to_owned();
    let mut session_key = "claude-code:sess-1".to_owned();
    let mut agent = "claude".to_owned();
    let mut message = CaptureMessage {
        event_id: "evt-1".to_owned(),
        role: Role::Assistant,
        content: "hello".to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    };
    let mut batch = CaptureBatchRequest {
        session_key: String::new(),
        agent: String::new(),
        channel: Some("claude-code".to_owned()),
        title: Some("a session".to_owned()),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: None,
        updated_at: None,
        messages: Vec::new(),
    };
    match position {
        Position::Content => text.clone_into(&mut message.content),
        Position::ToolResult => message.tool_result = Some(text.to_owned()),
        Position::ToolArgsValue => {
            message.tool_args = Some(object_of(&json!({
                "cmd": {"argv": ["sh", "-c", text], "opts": {"note": text}},
                "nest": [{"deep": {"text": text}}],
            }))?);
        }
        Position::ToolArgsKey => message.tool_args = Some(object_of(&json!({ text: "x" }))?),
        Position::ToolArgsJson => message.tool_args = Some(object(text)?),
        Position::MetadataValue => {
            message.metadata = Some(object_of(&json!({"ctx": {"note": text}}))?);
        }
        Position::MetadataKey => message.metadata = Some(object_of(&json!({ text: 1 }))?),
        Position::Title => batch.title = Some(text.to_owned()),
        Position::SettingsValue => {
            batch.settings = Some(object_of(&json!({"profile": {"note": text}}))?);
        }
        Position::SessionKey => text.clone_into(&mut session_key),
        Position::Channel => batch.channel = Some(text.to_owned()),
        Position::TaskId => batch.task_id = Some(text.to_owned()),
        Position::ToolName => message.tool_name = Some(text.to_owned()),
        Position::Agent => text.clone_into(&mut agent),
        Position::OriginNode => text.clone_into(&mut node),
        Position::EventId => text.clone_into(&mut message.event_id),
    }
    batch.session_key.clone_from(&session_key);
    batch.agent.clone_from(&agent);
    batch.messages = vec![message];
    let session = BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: node.clone(),
            conversation_id: "conv-1".to_owned(),
            session_key,
            agent,
            created_at: None,
            updated_at: None,
        },
        batch,
    };
    Ok(capture::bundle::seal(
        BundleAuthor {
            user: "phil".to_owned(),
            node,
        },
        at()?,
        Sanitization::Raw,
        vec![session],
    )?
    .bundle)
}

fn survivors(
    name: &str,
    position: Position,
    carrier: &str,
    planted: &[&str],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let input = bundle_with(position, &fill(carrier, planted))?;
    let before = serde_json::to_string(&input)?;
    for value in planted {
        assert!(
            before.contains(value),
            "{name} at {position:?}: the input bundle must plant {value}"
        );
    }
    let sanitized = match sanitize::run(&input, &Policy::default(), at()?) {
        Ok(sanitized) => sanitized,
        Err(error) => {
            return Ok(vec![format!(
                "{name} | {position:?} | run refused: {error}"
            )]);
        }
    };
    let file = String::from_utf8(sanitized.sealed.text.clone())?;
    let report = sanitized.report.markdown("s.clean.json", "no rules");
    let mut found = Vec::new();
    for value in planted {
        if file.contains(value) {
            found.push(format!("{name} | {position:?} | sealed text | {value}"));
        }
        if report.contains(value) {
            found.push(format!("{name} | {position:?} | report | {value}"));
        }
    }
    Ok(found)
}

fn no_shape_survives(position: Position) -> TestResult {
    let mut found = Vec::new();
    for shape in SHAPES {
        found.extend(survivors(
            shape.name,
            position,
            shape.carrier,
            shape.planted,
        )?);
    }
    assert!(
        found.is_empty(),
        "{} survivors across {} shapes at {position:?}\nshape | position | where | value\n{}",
        found.len(),
        SHAPES.len(),
        found.join("\n")
    );
    Ok(())
}

#[test]
fn placement_cases_leave_no_survivor() -> TestResult {
    let mut found = Vec::new();
    for case in CASES {
        found.extend(survivors(
            case.name,
            case.position,
            case.carrier,
            case.planted,
        )?);
    }
    assert!(
        found.is_empty(),
        "{} survivors across {} placement cases\ncase | position | where | value\n{}",
        found.len(),
        CASES.len(),
        found.join("\n")
    );
    Ok(())
}

position_tests! {
    shapes_in_content_leave_no_survivor => Position::Content;
    shapes_in_tool_result_leave_no_survivor => Position::ToolResult;
    shapes_in_nested_tool_args_values_leave_no_survivor => Position::ToolArgsValue;
    shapes_in_tool_args_keys_leave_no_survivor => Position::ToolArgsKey;
    shapes_in_metadata_values_leave_no_survivor => Position::MetadataValue;
    shapes_in_metadata_keys_leave_no_survivor => Position::MetadataKey;
    shapes_in_titles_leave_no_survivor => Position::Title;
    shapes_in_settings_values_leave_no_survivor => Position::SettingsValue;
    shapes_in_session_keys_leave_no_survivor => Position::SessionKey;
    shapes_in_channels_leave_no_survivor => Position::Channel;
    shapes_in_task_ids_leave_no_survivor => Position::TaskId;
    shapes_in_tool_names_leave_no_survivor => Position::ToolName;
    shapes_in_agents_leave_no_survivor => Position::Agent;
    shapes_in_origin_nodes_leave_no_survivor => Position::OriginNode;
    shapes_in_event_ids_leave_no_survivor => Position::EventId;
}
