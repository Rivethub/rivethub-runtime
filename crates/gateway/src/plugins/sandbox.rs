use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use strum::IntoEnumIterator as _;
use wire::plugin::manifest::Permission;

const RESOLVER: &str = "/etc/resolv.conf";

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter)]
enum SystemPath {
    Usr,
    Bin,
    Sbin,
    Lib,
    Lib32,
    Lib64,
    Etc,
    Proc,
    Sys,
    DevNull,
    DevZero,
    DevRandom,
    DevUrandom,
}

impl SystemPath {
    const fn path(self) -> &'static str {
        match self {
            Self::Usr => "/usr",
            Self::Bin => "/bin",
            Self::Sbin => "/sbin",
            Self::Lib => "/lib",
            Self::Lib32 => "/lib32",
            Self::Lib64 => "/lib64",
            Self::Etc => "/etc",
            Self::Proc => "/proc",
            Self::Sys => "/sys",
            Self::DevNull => "/dev/null",
            Self::DevZero => "/dev/zero",
            Self::DevRandom => "/dev/random",
            Self::DevUrandom => "/dev/urandom",
        }
    }

    const fn writable(self) -> bool {
        matches!(self, Self::DevNull)
    }
}

pub(super) const HOST_PERMISSIONS: [Permission; 4] = [
    Permission::HostFilesRead,
    Permission::HostFilesWrite,
    Permission::HostNetwork,
    Permission::HostExec,
];

#[derive(Debug, Clone)]
pub(super) struct Folders {
    pub(super) install: PathBuf,
    pub(super) data: PathBuf,
    pub(super) run: PathBuf,
    pub(super) api: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct Request {
    pub(super) permissions: Vec<Permission>,
    pub(super) folders: Folders,
    pub(super) program: PathBuf,
    pub(super) search: Option<std::ffi::OsString>,
    pub(super) node_ports: Vec<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Policy {
    pub(super) read: Vec<PathBuf>,
    pub(super) write: Vec<PathBuf>,
    pub(super) own: Vec<PathBuf>,
    pub(super) connect: Vec<PathBuf>,
    pub(super) network: bool,
    pub(super) node_ports: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Confinement {
    Full,
    Partial,
    Unconfined,
}

impl Confinement {
    pub(super) const fn describe(self) -> &'static str {
        match self {
            Self::Full => "confined by Landlock",
            Self::Partial => "partly confined by Landlock: this kernel lacks some of its rules",
            Self::Unconfined => "not confined: this kernel has no Landlock",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Support {
    Full,
    Partial,
    Missing,
}

fn asks_host(requested: &[Permission]) -> Option<Permission> {
    requested
        .iter()
        .copied()
        .find(|permission| HOST_PERMISSIONS.contains(permission))
}

pub(super) fn refusal(requested: &[Permission], support: Support) -> Option<String> {
    let permission = asks_host(requested)?;
    match support {
        Support::Full => None,
        Support::Partial => Some(format!(
            "this node's kernel enforces only part of Landlock, so rh cannot fully confine a plug-in that asks for {permission}; it cannot be enabled here"
        )),
        Support::Missing => Some(format!(
            "this node's kernel has no Landlock, so rh cannot confine a plug-in that asks for {permission}; it cannot be enabled here"
        )),
    }
}

pub(super) fn admit(request: &Request, confinement: Confinement) -> Result<Confinement, String> {
    let support = match confinement {
        Confinement::Full => Support::Full,
        Confinement::Partial => Support::Partial,
        Confinement::Unconfined => Support::Missing,
    };
    refusal(&request.permissions, support).map_or(Ok(confinement), Err)
}

#[must_use]
pub(super) fn policy(
    permissions: &[Permission],
    folders: &Folders,
    program: Option<&Path>,
    resolver: Option<&Path>,
    node_ports: &[u16],
) -> Policy {
    let granted = |permission| permissions.contains(&permission);
    let network = granted(Permission::HostNetwork);
    let mut read: Vec<PathBuf> = SystemPath::iter()
        .map(|system| PathBuf::from(system.path()))
        .collect();
    read.push(folders.install.clone());
    if let Some(program) = program {
        read.push(program.to_path_buf());
        if let Some(prefix) = installation(program) {
            read.push(prefix);
        }
    }
    if network && let Some(resolver) = resolver {
        read.push(resolver.to_path_buf());
    }
    if granted(Permission::HostFilesRead) {
        read.push(PathBuf::from("/"));
    }
    let mut write: Vec<PathBuf> = SystemPath::iter()
        .filter(|system| system.writable())
        .map(|system| PathBuf::from(system.path()))
        .collect();
    if granted(Permission::HostFilesWrite) {
        write.push(PathBuf::from("/"));
    }
    Policy {
        read,
        write,
        own: vec![folders.data.clone(), folders.run.clone()],
        connect: vec![folders.api.clone()],
        network,
        node_ports: if network {
            node_ports.to_vec()
        } else {
            Vec::new()
        },
    }
}

fn installation(program: &Path) -> Option<PathBuf> {
    let folder = program.parent()?;
    (folder.file_name() == Some(OsStr::new("bin")))
        .then(|| folder.parent().map(|prefix| prefix.join("lib")))
        .flatten()
}

fn located(program: &Path, install: &Path, search: Option<&OsStr>) -> Result<PathBuf, String> {
    if program.components().count() > 1 {
        let found = std::fs::canonicalize(program)
            .map_err(|error| format!("{}: {error}", program.display()))?;
        return if found.starts_with(install) {
            Ok(found)
        } else {
            Err(format!(
                "{} resolves outside the plug-in's folder",
                program.display()
            ))
        };
    }
    let found = search
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|folder| folder.join(program))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("{} is not on PATH", program.display()))?;
    std::fs::canonicalize(&found).map_err(|error| format!("{}: {error}", found.display()))
}

pub(super) fn resolve(request: &Request) -> Result<Policy, String> {
    let program = located(
        &request.program,
        &request.folders.install,
        request.search.as_deref(),
    )?;
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let resolver = std::fs::canonicalize(RESOLVER).ok();
    Ok(policy(
        &request.permissions,
        &request.folders,
        Some(&program),
        resolver.as_deref(),
        &request.node_ports,
    ))
}

#[cfg(target_os = "linux")]
mod landlock_rules {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible as _, NetPort, Ruleset,
        RulesetAttr as _, RulesetCreatedAttr as _, RulesetError, RulesetStatus, Scope,
        path_beneath_rules,
    };

    use super::{Confinement, Policy, Support};

    const ABI_KNOWN: ABI = ABI::V9;

    pub(in crate::plugins) fn support() -> Support {
        let full = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI_KNOWN))
            .and_then(|ruleset| ruleset.handle_access(AccessNet::from_all(ABI_KNOWN)))
            .and_then(|ruleset| ruleset.scope(Scope::from_all(ABI_KNOWN)))
            .and_then(Ruleset::create)
            .is_ok();
        let some = || {
            Ruleset::default()
                .set_compatibility(CompatLevel::HardRequirement)
                .handle_access(AccessFs::from_all(ABI::V1))
                .and_then(Ruleset::create)
                .is_ok()
        };
        if full {
            Support::Full
        } else if some() {
            Support::Partial
        } else {
            Support::Missing
        }
    }

    pub(in crate::plugins) fn restrict(policy: &Policy) -> Result<Confinement, RulesetError> {
        let abi = ABI_KNOWN;
        let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(abi))?;
        if !policy.network {
            ruleset = ruleset.handle_access(AccessNet::from_all(abi))?;
        } else if !policy.node_ports.is_empty() {
            ruleset = ruleset.handle_access(AccessNet::ConnectTcp)?;
        }
        let open = (1..=u16::MAX)
            .filter(|port| {
                policy.network && !policy.node_ports.is_empty() && !policy.node_ports.contains(port)
            })
            .map(|port| Ok::<_, RulesetError>(NetPort::new(port, AccessNet::ConnectTcp)));
        let scope = if policy.network {
            Scope::Signal.into()
        } else {
            Scope::from_all(abi)
        };
        let status = ruleset
            .scope(scope)?
            .create()?
            .add_rules(path_beneath_rules(&policy.read, AccessFs::from_read(abi)))?
            .add_rules(path_beneath_rules(&policy.write, AccessFs::from_write(abi)))?
            .add_rules(path_beneath_rules(&policy.own, AccessFs::from_all(abi)))?
            .add_rules(path_beneath_rules(&policy.connect, AccessFs::ResolveUnix))?
            .add_rules(open)?
            .restrict_self()?;
        Ok(match status.ruleset {
            RulesetStatus::FullyEnforced => Confinement::Full,
            RulesetStatus::PartiallyEnforced => Confinement::Partial,
            RulesetStatus::NotEnforced => Confinement::Unconfined,
        })
    }
}

#[cfg(target_os = "linux")]
pub(super) use landlock_rules::{restrict, support};

#[cfg(not(target_os = "linux"))]
pub(super) const fn support() -> Support {
    Support::Missing
}

#[cfg(not(target_os = "linux"))]
pub(super) fn restrict(_policy: &Policy) -> Result<Confinement, std::convert::Infallible> {
    Ok(Confinement::Unconfined)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use wire::plugin::manifest::Permission;

    use super::{Folders, Request, Support, policy, refusal, resolve};

    fn folders(install: &Path) -> Folders {
        Folders {
            install: install.to_path_buf(),
            data: install.join("data"),
            run: install.join("run"),
            api: install.join("api.sock"),
        }
    }

    #[test]
    fn a_program_in_a_bin_folder_reads_only_the_lib_beside_it() {
        let install = Path::new("/srv/plugin");
        let read = |program: &str| {
            policy(&[], &folders(install), Some(Path::new(program)), None, &[]).read
        };
        let cargo = read("/home/someone/.cargo/bin/cargo");
        assert!(cargo.contains(&PathBuf::from("/home/someone/.cargo/lib")));
        assert!(!cargo.contains(&PathBuf::from("/home/someone/.cargo")));
        let home = read("/home/someone/bin/tool");
        assert!(!home.contains(&PathBuf::from("/home/someone")));
        assert!(!read("/bin/sh").contains(&PathBuf::from("/")));
    }

    #[test]
    fn a_program_path_must_stay_in_the_install_folder() {
        let root = tempfile::tempdir().unwrap();
        let install = std::fs::canonicalize(root.path()).unwrap();
        std::os::unix::fs::symlink("/bin/sh", install.join("escape")).unwrap();
        let request = Request {
            permissions: Vec::new(),
            folders: folders(&install),
            program: install.join("escape"),
            search: None,
            node_ports: Vec::new(),
        };
        assert!(resolve(&request).unwrap_err().contains("outside"));
    }

    #[test]
    fn a_networked_plugin_is_kept_off_the_nodes_own_ports() {
        let install = Path::new("/srv/plugin");
        let networked = policy(
            &[Permission::HostNetwork],
            &folders(install),
            None,
            None,
            &[5174, 5175],
        );
        assert!(networked.network);
        assert_eq!(networked.node_ports, [5174, 5175]);
        let offline = policy(&[], &folders(install), None, None, &[5174]);
        assert!(!offline.network && offline.node_ports.is_empty());
    }

    #[test]
    fn host_permissions_need_every_landlock_rule() {
        let plain = [Permission::SessionsRead];
        let host = [Permission::SessionsRead, Permission::HostNetwork];
        assert_eq!(refusal(&plain, Support::Partial), None);
        assert_eq!(refusal(&host, Support::Full), None);
        assert!(refusal(&host, Support::Partial).is_some());
        assert!(refusal(&host, Support::Missing).is_some());
    }
}
