use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use jiff::Timestamp;
use tempfile::TempDir;

use super::resolve_host_zone;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const NEW_YORK: &str = "America/New_York";
const LOS_ANGELES: &str = "America/Los_Angeles";
const LOCALTIME_TARGET: &str = "../usr/share/zoneinfo/America/New_York";

fn write(root: &Path, path: &str, bytes: &[u8]) -> TestResult {
    let path = root.join(path);
    fs::create_dir_all(path.parent().ok_or("fixture parent")?)?;
    Ok(fs::write(path, bytes)?)
}

fn write_zone(root: &Path, path: &str, name: &str) -> TestResult {
    let (_, bytes) = jiff_tzdb::get(name).ok_or("bundled fixture")?;
    write(root, path, bytes)
}

fn zones() -> TestResult<TempDir> {
    let root = tempfile::tempdir()?;
    fs::create_dir(root.path().join("etc"))?;
    write_zone(root.path(), "usr/share/zoneinfo/America/New_York", NEW_YORK)?;
    write_zone(
        root.path(),
        "usr/share/zoneinfo/America/Los_Angeles",
        LOS_ANGELES,
    )?;
    Ok(root)
}

fn offset(root: &Path, tz: Option<&str>) -> TestResult<i32> {
    let instant: Timestamp = "2026-03-08T06:00:00Z".parse()?;
    Ok(resolve_host_zone(root, tz).to_offset(instant).seconds())
}

#[test]
fn host_zone_matches_legacy_layout_and_tz_matrix() -> TestResult {
    let layouts = [
        "symlink",
        "regular",
        "timezone-only",
        "both-disagree",
        "neither",
    ];
    let cases = [
        (None, [-18000, -18000, -18000, -18000, 0]),
        (Some(""), [0; 5]),
        (Some("localtime"), [-18000, -18000, -18000, -18000, 0]),
        (Some(LOS_ANGELES), [-28800; 5]),
        (Some(":America/Los_Angeles"), [-28800; 5]),
        (Some("/tmp/probe/custom.tzif"), [3600; 5]),
        (Some(":/tmp/probe/custom.tzif"), [3600; 5]),
        (Some("/tmp/probe/zoneinfo/America/Los_Angeles"), [-18000; 5]),
        (
            Some(":/tmp/probe/zoneinfo/America/Los_Angeles"),
            [-18000; 5],
        ),
        (Some("Test/relative.tzif"), [32400; 5]),
        (Some("No/Such/Zone"), [-18000, 0, -18000, -18000, 0]),
    ];
    let mut fixtures = Vec::new();
    for layout in layouts {
        let fixture = zones()?;
        let root = fixture.path();
        write_zone(root, "tmp/probe/custom.tzif", "Europe/Paris")?;
        write_zone(root, "tmp/probe/zoneinfo/America/Los_Angeles", NEW_YORK)?;
        write_zone(root, "share/zoneinfo/Test/relative.tzif", "Asia/Tokyo")?;
        write_zone(root, "etc/zoneinfo/Test/relative.tzif", LOS_ANGELES)?;
        if matches!(layout, "symlink" | "both-disagree") {
            symlink(LOCALTIME_TARGET, root.join("etc/localtime"))?;
        }
        if layout == "regular" {
            write_zone(root, "etc/localtime", NEW_YORK)?;
        }
        if layout == "timezone-only" {
            write(root, "etc/timezone", b"America/New_York\t \n")?;
        }
        if layout == "both-disagree" {
            write(root, "etc/timezone", b"America/Los_Angeles\t \n")?;
        }
        fixtures.push((layout, fixture));
    }
    for (tz, expected) in cases {
        for ((layout, fixture), expected) in fixtures.iter().zip(expected) {
            assert_eq!(offset(fixture.path(), tz)?, expected, "{layout}, TZ={tz:?}");
        }
    }
    Ok(())
}

#[test]
fn host_zone_preserves_fallback_and_posix_lookup_order() -> TestResult {
    let fixture = zones()?;
    let root = fixture.path();
    let unknown = Some("No/Such/Zone");

    assert_eq!(offset(root, Some(" JST-9 "))?, 32400);
    assert_eq!(offset(root, Some(":JST-9"))?, 0);
    assert_eq!(offset(root, Some("ABC-0001:2:3"))?, 3723);
    assert_eq!(
        offset(root, Some("EST5EDT,M03.02.0/02:0:0,M11.1.0/2"))?,
        -18000
    );
    assert_eq!(offset(root, Some("EST5EDT,M3.2.0/-2,M11.1.0/2"))?, 0);
    assert_eq!(offset(root, Some("EST5EDT"))?, 0);

    write_zone(root, "usr/share/zoneinfo/America/Los_Angeles", NEW_YORK)?;
    assert_eq!(offset(root, Some(LOS_ANGELES))?, -18000);
    write_zone(root, "share/zoneinfo/Test/relative.tzif", "Asia/Tokyo")?;
    write(
        root,
        "usr/share/zoneinfo/Test/relative.tzif",
        b"broken TZif",
    )?;
    assert_eq!(offset(root, Some("Test/relative.tzif"))?, 0);
    write_zone(root, "usr/share/zoneinfo/America/Los_Angeles", LOS_ANGELES)?;

    write(
        root,
        "etc/config/system",
        b"config system\n\toption timezone 'America/Los_Angeles'\n\toption zonename \"America/New_York\" # comment\n",
    )?;
    assert_eq!(offset(root, unknown)?, -18000);

    write_zone(root, "etc/localtime", NEW_YORK)?;
    write(root, "etc/timezone", b"America/Los_Angeles\t \n")?;
    assert_eq!(offset(root, unknown)?, -28800);

    fs::remove_file(root.join("etc/localtime"))?;
    symlink(
        "../usr/share/zoneinfo/No/Such/Zone",
        root.join("etc/localtime"),
    )?;
    assert_eq!(offset(root, unknown)?, 0);
    Ok(())
}
