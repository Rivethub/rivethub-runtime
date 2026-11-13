use super::super::entity_match::TypeRoot;
use super::super::entity_match::type_root;
use super::super::entity_pass::compact_key;

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
pub(super) const FILE_SUFFIXES: &[&str] = &[
    "cfg", "conf", "css", "csv", "env", "html", "ini", "js", "json", "jsonl", "jsx", "lock", "log",
    "md", "parquet", "py", "rs", "sh", "sql", "toml", "ts", "tsx", "txt", "xml", "yaml", "yml",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
pub(super) const TOOLS: &[&str] = &[
    "bash",
    "edit",
    "glob",
    "imagegen",
    "memoryrecall",
    "memorysearch",
    "multiedit",
    "notebookedit",
    "pushnotification",
    "read",
    "taskcreate",
    "taskget",
    "tasklist",
    "taskoutput",
    "taskstop",
    "taskupdate",
    "todowrite",
    "toolsearch",
    "webfetch",
    "webresearch",
    "websearch",
    "write",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
pub(super) const CLIS: &[&str] = &[
    "apt",
    "awk",
    "bash",
    "cargo",
    "cat",
    "chmod",
    "chown",
    "cmake",
    "cp",
    "curl",
    "date",
    "diffstat",
    "dmesg",
    "dmsetup",
    "du",
    "echo",
    "expo",
    "exportfs",
    "find",
    "fly",
    "free",
    "fuser",
    "gcc",
    "git",
    "go",
    "grep",
    "gzip",
    "head",
    "htop",
    "ifconfig",
    "java",
    "journalctl",
    "kill",
    "killall",
    "ln",
    "ls",
    "lsblk",
    "lsof",
    "lspci",
    "make",
    "mkdir",
    "mount",
    "mv",
    "nano",
    "nfsd",
    "ninja",
    "node",
    "nohup",
    "npm",
    "nvidiasmi",
    "pct",
    "perl",
    "pgcontroldata",
    "pgdump",
    "pgrep",
    "ping",
    "pip",
    "pip3",
    "pkill",
    "ps",
    "python",
    "python3",
    "rg",
    "rm",
    "rsync",
    "ruby",
    "rustc",
    "scp",
    "screen",
    "sed",
    "sleep",
    "ss",
    "ssh",
    "sshd",
    "strace",
    "sudo",
    "systemctl",
    "tail",
    "tar",
    "tee",
    "tmux",
    "top",
    "touch",
    "uname",
    "useradd",
    "uv",
    "vim",
    "watch",
    "wget",
    "which",
    "whoami",
    "xargs",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
pub(super) const GENERICS: &[&str] = &[
    "command", "config", "data", "default", "false", "file", "index", "input", "kernel", "log",
    "logs", "none", "null", "output", "path", "root", "session", "summary", "temp", "tmp", "tool",
    "true",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    Keep,
    Drop(DropKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    Path,
    Address,
    Index,
    Tool,
    Cli,
    Generic,
}

#[must_use]
pub fn admit_name(name: &str) -> Admit {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Admit::Drop(DropKind::Generic);
    }
    if is_address(trimmed) {
        return Admit::Drop(DropKind::Address);
    }
    if is_path(trimmed) {
        return Admit::Drop(DropKind::Path);
    }
    if is_index(trimmed) {
        return Admit::Drop(DropKind::Index);
    }
    let key = compact_key(trimmed);
    if listed(&key, TOOLS) {
        return Admit::Drop(DropKind::Tool);
    }
    if listed(&key, CLIS) {
        return Admit::Drop(DropKind::Cli);
    }
    if listed(&key, GENERICS) {
        return Admit::Drop(DropKind::Generic);
    }
    Admit::Keep
}

#[must_use]
pub fn identifying_aliases(type_name: &str, value: &str) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(address) = first_ipv4(value) {
        push_unique(&mut found, address);
    }
    for token in value.split_whitespace() {
        let token = token.trim_matches(trim_token);
        if token.is_empty() {
            continue;
        }
        if is_address(token) {
            continue;
        }
        if looks_like_sku(token) {
            push_unique(&mut found, token.to_owned());
            if type_root(type_name) == TypeRoot::Host
                && let Some(tail) = last_alpha(token)
                && tail.len() >= 4
                && matches!(admit_name(&tail), Admit::Keep)
            {
                push_unique(&mut found, tail);
            }
        }
    }
    found
}

pub(super) fn looks_like_sku(name: &str) -> bool {
    let trimmed = name.trim();
    let chars = trimmed.chars().count();
    if !(8..=80).contains(&chars) {
        return false;
    }
    trimmed.chars().any(|c| c.is_ascii_digit())
        && trimmed.contains('-')
        && trimmed.chars().filter(char::is_ascii_alphabetic).count() >= 2
}
fn last_alpha(token: &str) -> Option<String> {
    let mut tail = None;
    let mut current = String::new();
    for ch in token.chars() {
        if ch.is_ascii_alphabetic() {
            current.push(ch);
        } else if !current.is_empty() {
            tail = Some(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tail = Some(current);
    }
    tail
}

pub(super) fn first_ipv4(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find(|ch: char| ch.is_ascii_digit()) {
        let tail = rest.get(start..)?;
        let end = tail
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-' && ch != '/')
            .unwrap_or(tail.len());
        let Some(token) = tail.get(..end) else {
            break;
        };
        if is_address(token) {
            return Some(canonical_ip(token));
        }
        let Some(next) = tail.get(token.len()..) else {
            break;
        };
        rest = next;
        if rest.is_empty() {
            break;
        }
    }
    None
}

pub(super) fn canonical_ip(token: &str) -> String {
    let body = token.split('/').next().unwrap_or(token);
    body.split(['.', '-'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

pub(super) fn is_address(name: &str) -> bool {
    let trimmed = name.trim();
    let (body, prefixed) = match trimmed.split_once('/') {
        Some((body, prefix)) => {
            let legal = !prefix.is_empty()
                && prefix.len() <= 3
                && prefix.chars().all(|ch| ch.is_ascii_digit());
            if !legal {
                return false;
            }
            (body, true)
        }
        None => (trimmed, false),
    };
    if body.is_empty()
        || body
            .chars()
            .any(|ch| !(ch.is_ascii_digit() || ch == '.' || ch == '-'))
    {
        return false;
    }
    let groups: Vec<&str> = body
        .split(['.', '-'])
        .filter(|part| !part.is_empty())
        .collect();
    let octets = groups.len() == 4
        && groups
            .iter()
            .all(|part| part.len() <= 3 && part.chars().all(|ch| ch.is_ascii_digit()));
    octets && (prefixed || body.contains('.') || body.matches('-').count() == 3)
}

fn is_index(name: &str) -> bool {
    if name.chars().count() == 1 {
        return true;
    }
    let has_letter = name.chars().any(|ch| ch.is_ascii_alphabetic());
    let has_digit = name.chars().any(|ch| ch.is_ascii_digit());
    if has_digit && !has_letter {
        return true;
    }
    device_index(&name.trim().to_ascii_lowercase())
}

fn device_index(lower: &str) -> bool {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            strset,
            reason = "free-text vocabulary matched against transcripts, not a protocol enum"
        )
    )]
    const PREFIXES: &[&str] = &[
        "gpu", "disk", "cpu", "nvme", "core", "slot", "cuda", "numa", "eth", "wlan", "br",
    ];
    PREFIXES.iter().any(|prefix| {
        lower.strip_prefix(prefix).is_some_and(|rest| {
            let rest = rest.trim_start_matches(['-', '_']);
            !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit())
        })
    })
}

fn is_repo(name: &str) -> bool {
    let Some((owner, repo)) = name.split_once('/') else {
        return false;
    };
    let part = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    !repo.contains('/')
        && part(owner)
        && part(repo)
        && !owner.starts_with('.')
        && !repo.rsplit_once('.').is_some_and(|(_, ext)| file_ext(ext))
}

fn is_path(name: &str) -> bool {
    if is_repo(name) {
        return false;
    }
    if name.contains('/') || name.contains('\\') || name.contains("://") || name.starts_with('~') {
        return true;
    }
    if let Some((_, ext)) = name.rsplit_once('.')
        && file_ext(ext)
    {
        return true;
    }
    name.rsplit_once('-')
        .is_some_and(|(_, tail)| file_ext(tail))
}

fn file_ext(ext: &str) -> bool {
    let ext = ext.trim().to_ascii_lowercase();
    (1..=8).contains(&ext.len())
        && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
        && listed(&ext, FILE_SUFFIXES)
}

fn listed(key: &str, list: &[&str]) -> bool {
    list.binary_search(&key).is_ok()
}

fn trim_token(ch: char) -> bool {
    !ch.is_ascii_alphanumeric() && ch != '-' && ch != '.'
}

fn push_unique(found: &mut Vec<String>, value: String) {
    if !found.iter().any(|have| have.eq_ignore_ascii_case(&value)) {
        found.push(value);
    }
}
