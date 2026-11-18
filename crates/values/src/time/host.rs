use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use jiff::tz::TimeZone;

mod posix;

const ZONEINFO: &str = "usr/share/zoneinfo";

pub fn resolve_host_zone(root: &Path, tz: Option<&str>) -> TimeZone {
    let zone = match tz {
        None | Some("localtime") => read_path(root, Path::new("/etc/localtime")),
        Some("") => Some(TimeZone::UTC),
        Some(tz) => {
            if let Some(path) = tz.strip_prefix(':') {
                zone_file(root, Path::new(path)).and_then(read_tzif)
            } else {
                match zone_file(root, Path::new(tz)) {
                    Some(file) => read_tzif(file),
                    None => posix::parse(tz.trim_ascii()),
                }
            }
        }
    };
    match zone.or_else(|| {
        let name = system_name(root)?;
        read_path(root, Path::new(&format!("/{ZONEINFO}/{name}")))
    }) {
        Some(zone) => zone,
        None => TimeZone::UTC,
    }
}

fn zone_file(root: &Path, path: &Path) -> Option<File> {
    if path.is_absolute() {
        return opened(root, path);
    }
    [
        Path::new(ZONEINFO),
        Path::new("share/zoneinfo"),
        Path::new("etc/zoneinfo"),
        Path::new("usr/share/lib/zoneinfo"),
    ]
    .into_iter()
    .find_map(|directory| opened(root, &directory.join(path)))
}

fn opened(root: &Path, path: &Path) -> Option<File> {
    let relative = match path.strip_prefix("/") {
        Ok(relative) => relative,
        Err(_relative) => path,
    };
    if let Ok(file) = File::open(root.join(relative)) {
        return Some(file);
    }
    None
}

fn read_path(root: &Path, path: &Path) -> Option<TimeZone> {
    opened(root, path).and_then(read_tzif)
}

fn read_tzif(mut file: File) -> Option<TimeZone> {
    let mut bytes = Vec::new();
    match file.read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(_unreadable) => return None,
    }
    if let Ok(zone) = TimeZone::tzif("localtime", &bytes) {
        return Some(zone);
    }
    None
}

#[cfg(target_os = "macos")]
fn system_name(_root: &Path) -> Option<String> {
    if let Ok(name) = iana_time_zone::get_timezone() {
        return Some(name);
    }
    None
}

#[cfg(target_os = "linux")]
fn system_name(root: &Path) -> Option<String> {
    if let Ok(target) = std::fs::read_link(root.join("etc/localtime"))
        && let Some(target) = target.to_str()
        && let Some(name) = linked_zone_name(target)
    {
        return Some(name.to_owned());
    }
    if let Ok(contents) = std::fs::read_to_string(root.join("etc/timezone")) {
        return Some(contents.trim_end().to_owned());
    }
    if let Ok(name) = openwrt_name(opened(root, Path::new("/etc/config/system"))?) {
        return name;
    }
    None
}

#[cfg(target_os = "linux")]
fn linked_zone_name(target: &str) -> Option<&str> {
    let path = match target.strip_prefix("..") {
        Some(path) => path,
        None => target,
    };
    path.strip_prefix("/usr/share/zoneinfo/")
        .or_else(|| path.strip_prefix("/etc/zoneinfo/"))
}

#[cfg(target_os = "linux")]
fn openwrt_name(file: File) -> Result<Option<String>, OpenWrtConfigError> {
    use std::io::{BufRead as _, BufReader};

    let mut in_system = false;
    let mut timezone = None;
    for contents in BufReader::new(file).lines() {
        let contents = contents?;
        let mut line = contents.as_str();
        match word(&mut line)? {
            Some("config") => {
                in_system = word(&mut line)? == Some("system") && word(&mut line)?.is_none();
            }
            Some("option") if in_system => {
                let key = word(&mut line)?;
                if matches!(key, Some("zonename" | "timezone"))
                    && let Some(value) = word(&mut line)?
                    && word(&mut line)?.is_none()
                {
                    if key == Some("zonename") {
                        return Ok(Some(value.to_owned()));
                    }
                    timezone = Some(value.to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(timezone)
}

#[cfg(target_os = "linux")]
#[derive(Debug, thiserror::Error)]
enum OpenWrtConfigError {
    #[error("read zone configuration")]
    Read(#[from] std::io::Error),
    #[error("unclosed quote in zone configuration")]
    UnclosedQuote,
}

#[cfg(target_os = "linux")]
fn word<'a>(line: &mut &'a str) -> Result<Option<&'a str>, OpenWrtConfigError> {
    let text = line.trim_start();
    if text.is_empty() || text.starts_with('#') {
        *line = "";
        return Ok(None);
    }
    let (word, tail) = if let Some(quote @ ('\'' | '"')) = text.chars().next() {
        let after = text
            .strip_prefix(quote)
            .ok_or(OpenWrtConfigError::UnclosedQuote)?;
        after
            .split_once(quote)
            .ok_or(OpenWrtConfigError::UnclosedQuote)?
    } else {
        match text.split_once(char::is_whitespace) {
            Some(parts) => parts,
            None => (text, ""),
        }
    };
    *line = tail;
    Ok(Some(word))
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
