use crate::Error;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ours,
    Foreign,
    Absent,
}

const LISTEN: &str = "0A";

pub(crate) fn judge(tables: &[&str], port: u16, uid: u32) -> Verdict {
    let suffix = format!(":{port:04X}");
    let mut ours = false;
    for table in tables {
        for row in table.lines().skip(1) {
            let columns: Vec<&str> = row.split_whitespace().collect();
            let (Some(local), Some(state), Some(owner)) =
                (columns.get(1), columns.get(3), columns.get(7))
            else {
                continue;
            };
            if *state != LISTEN || !local.ends_with(&suffix) {
                continue;
            }
            if owner.parse::<u32>() == Ok(uid) {
                ours = true;
            } else {
                return Verdict::Foreign;
            }
        }
    }
    if ours { Verdict::Ours } else { Verdict::Absent }
}

#[cfg(target_os = "linux")]
async fn verdict(port: u16, uid: u32) -> Result<Verdict, Error> {
    let v4 = tokio::fs::read_to_string("/proc/net/tcp").await?;
    let v6 = match tokio::fs::read_to_string("/proc/net/tcp6").await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    Ok(judge(&[&v4, &v6], port, uid))
}

#[cfg(not(target_os = "linux"))]
async fn verdict(port: u16, uid: u32) -> Result<Verdict, Error> {
    let output = tokio::process::Command::new("lsof")
        .args(["-nP", "-a", "-t", "-u", &uid.to_string()])
        .arg(format!("-iTCP:{port}"))
        .args(["-sTCP:LISTEN"])
        .output()
        .await?;
    Ok(if output.stdout.iter().any(u8::is_ascii_digit) {
        Verdict::Ours
    } else {
        Verdict::Absent
    })
}

pub(crate) async fn require_ours(port: u16, uid: u32) -> Result<(), Error> {
    match verdict(port, uid).await? {
        Verdict::Ours => Ok(()),
        Verdict::Foreign | Verdict::Absent => Err(Error::ListenerNotOurs { port }),
    }
}

#[cfg(test)]
mod tests {
    use super::{Verdict, judge};

    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    fn table(rows: &[(&str, &str, u32)]) -> String {
        let body: Vec<String> = rows
            .iter()
            .map(|(local, state, uid)| {
                format!(
                    "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000  {uid}        0 1 1 0\n"
                )
            })
            .collect();
        format!("{HEADER}{}", body.concat())
    }

    #[test]
    fn a_listener_of_another_user_is_foreign() {
        let tcp = table(&[("0100007F:F572", "0A", 1001)]);
        assert_eq!(judge(&[&tcp], 62834, 1000), Verdict::Foreign);
    }

    #[test]
    fn our_listener_is_ours_and_a_shared_port_is_foreign() {
        let ours = table(&[("0100007F:F572", "0A", 1000)]);
        assert_eq!(judge(&[&ours], 62834, 1000), Verdict::Ours);
        let both = table(&[("0100007F:F572", "0A", 1000), ("00000000:F572", "0A", 0)]);
        assert_eq!(judge(&[&both], 62834, 1000), Verdict::Foreign);
    }

    #[test]
    fn other_ports_and_established_sockets_do_not_count() {
        let tcp = table(&[("0100007F:F572", "01", 1001), ("0100007F:0050", "0A", 1001)]);
        assert_eq!(judge(&[&tcp], 62834, 1000), Verdict::Absent);
    }
}
