use jiff::tz::TimeZone;

pub fn host_zone() -> TimeZone {
    #[cfg(unix)]
    {
        host_zone_with(std::path::Path::new("/etc/localtime"))
    }
    #[cfg(not(unix))]
    {
        TimeZone::system()
    }
}

#[cfg(unix)]
fn host_zone_with(localtime: &std::path::Path) -> TimeZone {
    match TimeZone::try_system() {
        Ok(zone) => zone,
        Err(_invalid) => match std::fs::read(localtime) {
            Ok(bytes) => match TimeZone::tzif("localtime", &bytes) {
                Ok(zone) => zone,
                Err(_invalid) => TimeZone::UTC,
            },
            Err(_unreadable) => TimeZone::UTC,
        },
    }
}

#[cfg(all(test, unix))]
#[test]
fn host_zone_resolves_tz_before_falling_back_to_localtime() -> Result<(), Box<dyn std::error::Error>>
{
    use jiff::Timestamp;
    use std::path::Path;
    use std::process::Command;

    const PROBE: &str = "host-zone-offset=";
    let at: Timestamp = "2026-03-08T06:00:00Z".parse()?;
    if let Some(expected) =
        std::env::args().find_map(|arg| arg.strip_prefix(PROBE).map(str::parse::<i32>))
    {
        let localtime = Path::new("/usr/share/zoneinfo/America/New_York");
        assert_eq!(host_zone_with(localtime).to_offset(at).seconds(), expected?);
        return Ok(());
    }
    let system = TimeZone::tzif("localtime", &std::fs::read("/etc/localtime")?)?;
    for (tz, offset) in [
        (None, system.to_offset(at).seconds()),
        (Some("America/Los_Angeles"), -28_800),
        (Some(":America/Los_Angeles"), -28_800),
        (Some("/usr/share/zoneinfo/America/Los_Angeles"), -28_800),
        (Some(":/usr/share/zoneinfo/America/Los_Angeles"), -28_800),
        (Some("localtime"), -18_000),
        (Some("No/Such/Zone"), -18_000),
        (Some(""), 0),
    ] {
        let mut child = Command::new(std::env::current_exe()?);
        child.args([
            "--exact",
            "time::host::host_zone_resolves_tz_before_falling_back_to_localtime",
            "--skip",
            &format!("{PROBE}{offset}"),
        ]);
        match tz {
            Some(name) => child.env("TZ", name),
            None => child.env_remove("TZ"),
        };
        let result = child.output()?;
        assert!(
            result.status.success(),
            "{tz:?}: {}",
            String::from_utf8_lossy(&result.stdout)
        );
    }
    Ok(())
}
