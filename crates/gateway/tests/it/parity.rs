use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Copy)]
struct OwnerSource {
    label: &'static str,
    text: &'static str,
    name: &'static str,
}

#[derive(Deserialize)]
struct Skip {
    id: String,
    reason: String,
}

fn sources() -> [OwnerSource; 2] {
    [
        OwnerSource {
            label: "replay",
            text: include_str!("../replay.rs"),
            name: "CLAIMS",
        },
        OwnerSource {
            label: "parity",
            text: include_str!("parity_play.rs"),
            name: "OWN",
        },
    ]
}

fn value_array<'a>(tail: &'a str, name: &str) -> Result<&'a str, String> {
    let Some(equals) = tail.find('=') else {
        return Err(format!("{name} missing assignment"));
    };
    let Some(after_eq) = tail.get(equals + 1..) else {
        return Err(format!("{name} missing assignment"));
    };
    let trimmed = after_eq.trim_start();
    let rest = match trimmed.strip_prefix('&') {
        Some(rest) => rest,
        None => trimmed,
    };
    let Some(body) = rest.strip_prefix('[') else {
        return Err(format!("{name} missing array"));
    };
    let Some(end) = body.find("];") else {
        return Err(format!("{name} missing close"));
    };
    body.get(..end)
        .ok_or_else(|| format!("{name} missing close"))
}

fn quoted_ids(body: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut rest = body;
    loop {
        let Some(start) = rest.find('"') else {
            return Ok(ids);
        };
        let Some(after) = rest.get(start + 1..) else {
            return Err("unclosed quote".to_owned());
        };
        let Some(end) = after.find('"') else {
            return Err("unclosed quote".to_owned());
        };
        let Some(text) = after.get(..end) else {
            return Err("unclosed quote".to_owned());
        };
        if text.contains('/') {
            ids.push(text.to_owned());
        }
        let Some(next) = after.get(end + 1..) else {
            return Ok(ids);
        };
        rest = next;
    }
}

fn const_ids(source: &str, name: &str) -> Result<Vec<String>, String> {
    let marker = format!("const {name}:");
    let Some(at) = source.find(&marker) else {
        return Err(format!("missing {name}"));
    };
    let Some(tail) = source.get(at..) else {
        return Err(format!("missing {name}"));
    };
    quoted_ids(value_array(tail, name)?)
}

fn insert_id(ids: &mut BTreeSet<String>, id: &str, label: &str) -> Result<(), String> {
    if !id.contains('/') {
        return Err(format!("{label} id {id}"));
    }
    if !ids.insert(id.to_owned()) {
        return Err(format!("{label} duplicates {id}"));
    }
    Ok(())
}

fn owner_ids() -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for source in sources() {
        for id in const_ids(source.text, source.name)? {
            insert_id(&mut ids, &id, source.label)?;
        }
    }
    Ok(ids)
}

fn skip_ids() -> Result<BTreeSet<String>, String> {
    let skips: Vec<Skip> = serde_json::from_str(include_str!("../fixtures/parity/skips.json"))
        .map_err(|error| format!("skip list: {error}"))?;
    let mut ids = BTreeSet::new();
    for skip in skips {
        if skip.reason.trim().is_empty() {
            return Err(format!("empty reason {}", skip.id));
        }
        insert_id(&mut ids, &skip.id, "skip")?;
    }
    Ok(ids)
}

fn wire_golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../wire/tests/golden")
        .join(name)
}

fn http_recordings() -> [PathBuf; 3] {
    [
        testkit::recording("gateway/http-plain.json"),
        wire_golden("gateway/http-tls.json"),
        wire_golden("term/http.json"),
    ]
}

fn ws_recordings() -> [PathBuf; 2] {
    [wire_golden("gateway/ws.json"), wire_golden("term/ws.json")]
}

fn recorded_ids() -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for path in http_recordings() {
        let cases =
            testkit::http::cases(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        for case in cases {
            insert_id(&mut ids, &case.id, "recorded")?;
        }
    }
    for path in ws_recordings() {
        let steps =
            testkit::ws::steps(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        for step in steps {
            insert_id(&mut ids, &step.id, "recorded")?;
        }
    }
    Ok(ids)
}

fn list_ids<'a>(label: &str, ids: impl Iterator<Item = &'a str>) -> String {
    let mut line = String::new();
    for id in ids {
        line.push_str(label);
        line.push(' ');
        line.push_str(id);
        line.push('\n');
    }
    line
}

fn partition(
    recorded: &BTreeSet<String>,
    replayed: &BTreeSet<String>,
    skipped: &BTreeSet<String>,
) -> Result<(), String> {
    let mut problems = list_ids(
        "extra replayed",
        replayed.difference(recorded).map(String::as_str),
    );
    problems.push_str(&list_ids(
        "extra skipped",
        skipped.difference(recorded).map(String::as_str),
    ));
    problems.push_str(&list_ids(
        "overlap",
        replayed.intersection(skipped).map(String::as_str),
    ));
    problems.push_str(&list_ids(
        "missing",
        recorded
            .iter()
            .filter(|id| !replayed.contains(*id) && !skipped.contains(*id))
            .map(String::as_str),
    ));
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[test]
fn every_recorded_case_has_one_owner_or_one_skip() {
    let recorded = match recorded_ids() {
        Ok(ids) => ids,
        Err(error) => panic!("{error}"),
    };
    let replayed = match owner_ids() {
        Ok(ids) => ids,
        Err(error) => panic!("{error}"),
    };
    let skipped = match skip_ids() {
        Ok(ids) => ids,
        Err(error) => panic!("{error}"),
    };
    if let Err(problems) = partition(&recorded, &replayed, &skipped) {
        panic!("{problems}");
    }
}
