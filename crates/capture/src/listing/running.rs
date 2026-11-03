use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeProcess {
    pid: u32,
    session_id: String,
    proc_start: Option<String>,
}

pub(crate) fn claude_running(root: &Path, native: &str) -> io::Result<Option<u32>> {
    let entries = match fs::read_dir(root.join("sessions")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        if let Some(process) = registered(&path)
            && process.session_id == native
            && alive(&process)
        {
            return Ok(Some(process.pid));
        }
    }
    Ok(None)
}

fn registered(path: &Path) -> Option<ClaudeProcess> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B12: a Claude process file being written or removed while it is read names no running session"
        )
    )]
    let process = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ClaudeProcess>(&bytes).ok());
    process
}

fn alive(process: &ClaudeProcess) -> bool {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B12: a pid with no process entry has exited"
        )
    )]
    let stat = fs::read_to_string(format!("/proc/{}/stat", process.pid)).ok();
    let started = stat
        .as_deref()
        .and_then(|stat| stat.rsplit_once(')'))
        .and_then(|(_, fields)| fields.split_whitespace().nth(19));
    match &process.proc_start {
        Some(recorded) => started == Some(recorded.as_str()),
        None => started.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::claude_running;

    #[test]
    fn a_session_runs_elsewhere_only_while_its_recorded_process_lives() {
        let root = tempfile::tempdir().expect("root");
        let sessions = root.path().join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        let own = std::process::id();
        let stat = std::fs::read_to_string(format!("/proc/{own}/stat")).expect("stat");
        let started = stat
            .rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .expect("start");
        let write = |pid: u32, session: &str, start: &str| {
            std::fs::write(
                sessions.join(format!("{pid}.json")),
                format!(r#"{{"pid":{pid},"sessionId":"{session}","procStart":"{start}"}}"#),
            )
            .expect("process file");
        };
        write(own, "live", started);
        write(4_000_000, "gone", "1");
        assert_eq!(
            claude_running(root.path(), "live").ok().flatten(),
            Some(own)
        );
        assert_eq!(claude_running(root.path(), "gone").ok().flatten(), None);
        write(own, "reused", "1");
        assert_eq!(claude_running(root.path(), "reused").ok().flatten(), None);
    }
}
