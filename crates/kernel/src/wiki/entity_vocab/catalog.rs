use serde::Serialize;

use super::super::entity_match::TypeRoot;
use super::super::entity_match::type_root;

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const HOST_KEYS: &[&str] = &[
    "identity",
    "ip",
    "network",
    "os",
    "kernel",
    "cpu",
    "ram",
    "gpus",
    "storage",
    "filesystems",
    "hypervisor",
    "containers",
    "services",
    "access",
    "bmc",
    "power",
    "location",
    "role",
    "state",
    "incidents",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const HARDWARE_KEYS: &[&str] = &[
    "identity",
    "vendor",
    "model",
    "serial",
    "interfaces",
    "capacity",
    "firmware",
    "host",
    "location",
    "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const SERVICE_KEYS: &[&str] = &[
    "unit",
    "host",
    "port",
    "command",
    "model",
    "engine",
    "config",
    "state",
    "dependencies",
    "consumers",
    "rollback",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const MODEL_KEYS: &[&str] = &[
    "identifier",
    "family",
    "base",
    "quantization",
    "size",
    "checkpoint",
    "served_by",
    "performance",
    "evaluation",
    "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const SOFTWARE_KEYS: &[&str] = &[
    "identity", "version", "purpose", "host", "command", "config", "repo", "language", "license",
    "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const REPO_KEYS: &[&str] = &[
    "identity", "host", "remote", "branch", "language", "license", "project", "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const PERSON_KEYS: &[&str] = &[
    "role",
    "contact",
    "devices",
    "preferences",
    "responsibilities",
    "relationships",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const ORG_KEYS: &[&str] = &[
    "identity",
    "members",
    "contact",
    "location",
    "relationships",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const PROJECT_KEYS: &[&str] = &[
    "identity", "goal", "status", "owners", "repos", "services", "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const PLACE_KEYS: &[&str] = &[
    "identity", "address", "location", "contains", "access", "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const AGENT_KEYS: &[&str] = &[
    "identity", "role", "host", "model", "tools", "config", "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const CONTAINER_KEYS: &[&str] = &[
    "identity", "image", "host", "command", "config", "ports", "state",
];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const DOMAIN_KEYS: &[&str] = &["identity", "registrar", "nameservers", "host", "state"];

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        strset,
        reason = "free-text vocabulary matched against transcripts, not a protocol enum"
    )
)]
const DATABASE_KEYS: &[&str] = &["identity", "engine", "host", "port", "size", "state"];

const HOST_SYNONYMS: &[(&str, &str)] = &[
    ("memory", "ram"),
    ("mem", "ram"),
    ("dram", "ram"),
    ("gpu", "gpus"),
    ("graphic", "gpus"),
    ("graphics", "gpus"),
    ("disk", "storage"),
    ("disks", "storage"),
    ("drive", "storage"),
    ("drives", "storage"),
    ("ssd", "storage"),
    ("hdd", "storage"),
    ("nvme", "storage"),
    ("size", "storage"),
    ("capacity", "storage"),
    ("filesystem", "filesystems"),
    ("fs", "filesystems"),
    ("mount", "filesystems"),
    ("mounts", "filesystems"),
    ("service", "services"),
    ("daemon", "services"),
    ("systemd", "services"),
    ("container", "containers"),
    ("docker", "containers"),
    ("pod", "containers"),
    ("pods", "containers"),
    ("nic", "network"),
    ("nics", "network"),
    ("interface", "network"),
    ("interfaces", "network"),
    ("ipaddress", "ip"),
    ("ips", "ip"),
    ("hostname", "identity"),
    ("fqdn", "identity"),
    ("chassis", "identity"),
    ("sku", "identity"),
    ("processor", "cpu"),
    ("processors", "cpu"),
    ("operatingsystem", "os"),
    ("distro", "os"),
    ("uname", "kernel"),
    ("ipmi", "bmc"),
    ("redfish", "bmc"),
    ("psu", "power"),
    ("purpose", "role"),
    ("deploy", "role"),
    ("deployment", "role"),
    ("status", "state"),
    ("incident", "incidents"),
    ("kvm", "hypervisor"),
    ("proxmox", "hypervisor"),
    ("ssh", "access"),
    ("login", "access"),
];

const SERVICE_SYNONYMS: &[(&str, &str)] = &[
    ("unitname", "unit"),
    ("servicename", "unit"),
    ("systemdunit", "unit"),
    ("service", "unit"),
    ("machine", "host"),
    ("node", "host"),
    ("listen", "port"),
    ("bind", "port"),
    ("ports", "port"),
    ("execstart", "command"),
    ("cmdline", "command"),
    ("models", "model"),
    ("servedmodel", "model"),
    ("servedmodels", "model"),
    ("commit", "engine"),
    ("binary", "engine"),
    ("enginecommit", "engine"),
    ("version", "engine"),
    ("dropin", "config"),
    ("dropins", "config"),
    ("override", "config"),
    ("environment", "config"),
    ("configfile", "config"),
    ("revert", "rollback"),
    ("rollbackcommand", "rollback"),
    ("dependency", "dependencies"),
    ("requires", "dependencies"),
    ("consumer", "consumers"),
    ("client", "consumers"),
    ("clients", "consumers"),
    ("status", "state"),
];

const MODEL_SYNONYMS: &[(&str, &str)] = &[
    ("id", "identifier"),
    ("modelid", "identifier"),
    ("modelname", "identifier"),
    ("architecture", "family"),
    ("basemodel", "base"),
    ("quant", "quantization"),
    ("quantisation", "quantization"),
    ("parameters", "size"),
    ("params", "size"),
    ("modelsize", "size"),
    ("weights", "checkpoint"),
    ("ckpt", "checkpoint"),
    ("servedby", "served_by"),
    ("benchmark", "performance"),
    ("bench", "performance"),
    ("perf", "performance"),
    ("eval", "evaluation"),
    ("score", "evaluation"),
    ("status", "state"),
];

const SOFTWARE_SYNONYMS: &[(&str, &str)] = &[
    ("ver", "version"),
    ("repository", "repo"),
    ("cmd", "command"),
    ("status", "state"),
];

const REPO_SYNONYMS: &[(&str, &str)] = &[
    ("defaultbranch", "branch"),
    ("url", "remote"),
    ("origin", "remote"),
    ("repository", "identity"),
    ("status", "state"),
];

const PERSON_SYNONYMS: &[(&str, &str)] = &[
    ("email", "contact"),
    ("phone", "contact"),
    ("device", "devices"),
    ("preference", "preferences"),
    ("responsibility", "responsibilities"),
    ("relationship", "relationships"),
    ("job", "role"),
];

const ORG_SYNONYMS: &[(&str, &str)] = &[
    ("member", "members"),
    ("people", "members"),
    ("email", "contact"),
    ("relationship", "relationships"),
];

const PROJECT_SYNONYMS: &[(&str, &str)] = &[
    ("owner", "owners"),
    ("repo", "repos"),
    ("repository", "repos"),
    ("service", "services"),
    ("purpose", "goal"),
];

const PLACE_SYNONYMS: &[(&str, &str)] = &[("addr", "address"), ("status", "state")];

const AGENT_SYNONYMS: &[(&str, &str)] = &[("tool", "tools"), ("llm", "model"), ("status", "state")];

const CONTAINER_SYNONYMS: &[(&str, &str)] = &[
    ("dockerimage", "image"),
    ("dropin", "config"),
    ("status", "state"),
];

const DOMAIN_SYNONYMS: &[(&str, &str)] = &[("dns", "nameservers"), ("status", "state")];

const DATABASE_SYNONYMS: &[(&str, &str)] = &[("db", "identity"), ("status", "state")];

pub(super) struct Vocab {
    pub(super) root: &'static str,
    pub(super) keys: &'static [&'static str],
    pub(super) synonyms: &'static [(&'static str, &'static str)],
}

const HOST: Vocab = Vocab {
    root: "host",
    keys: HOST_KEYS,
    synonyms: HOST_SYNONYMS,
};
const HARDWARE: Vocab = Vocab {
    root: "hardware",
    keys: HARDWARE_KEYS,
    synonyms: &[],
};
const SERVICE: Vocab = Vocab {
    root: "service",
    keys: SERVICE_KEYS,
    synonyms: SERVICE_SYNONYMS,
};
const MODEL: Vocab = Vocab {
    root: "model",
    keys: MODEL_KEYS,
    synonyms: MODEL_SYNONYMS,
};
const SOFTWARE: Vocab = Vocab {
    root: "software",
    keys: SOFTWARE_KEYS,
    synonyms: SOFTWARE_SYNONYMS,
};
const REPO: Vocab = Vocab {
    root: "repo",
    keys: REPO_KEYS,
    synonyms: REPO_SYNONYMS,
};
const PERSON: Vocab = Vocab {
    root: "person",
    keys: PERSON_KEYS,
    synonyms: PERSON_SYNONYMS,
};
const ORG: Vocab = Vocab {
    root: "organization",
    keys: ORG_KEYS,
    synonyms: ORG_SYNONYMS,
};
const PROJECT: Vocab = Vocab {
    root: "project",
    keys: PROJECT_KEYS,
    synonyms: PROJECT_SYNONYMS,
};
const PLACE: Vocab = Vocab {
    root: "place",
    keys: PLACE_KEYS,
    synonyms: PLACE_SYNONYMS,
};
const AGENT: Vocab = Vocab {
    root: "agent",
    keys: AGENT_KEYS,
    synonyms: AGENT_SYNONYMS,
};
const CONTAINER: Vocab = Vocab {
    root: "container",
    keys: CONTAINER_KEYS,
    synonyms: CONTAINER_SYNONYMS,
};
const DOMAIN: Vocab = Vocab {
    root: "domain",
    keys: DOMAIN_KEYS,
    synonyms: DOMAIN_SYNONYMS,
};
const DATABASE: Vocab = Vocab {
    root: "database",
    keys: DATABASE_KEYS,
    synonyms: DATABASE_SYNONYMS,
};
const GENERIC: Vocab = Vocab {
    root: "other",
    keys: &[],
    synonyms: &[],
};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct VocabEntry {
    #[serde(rename = "type")]
    pub root: &'static str,
    pub attributes: &'static [&'static str],
}

const VOCABULARY: &[VocabEntry] = &[
    VocabEntry {
        root: HOST.root,
        attributes: HOST.keys,
    },
    VocabEntry {
        root: HARDWARE.root,
        attributes: HARDWARE.keys,
    },
    VocabEntry {
        root: SERVICE.root,
        attributes: SERVICE.keys,
    },
    VocabEntry {
        root: MODEL.root,
        attributes: MODEL.keys,
    },
    VocabEntry {
        root: SOFTWARE.root,
        attributes: SOFTWARE.keys,
    },
    VocabEntry {
        root: REPO.root,
        attributes: REPO.keys,
    },
    VocabEntry {
        root: PERSON.root,
        attributes: PERSON.keys,
    },
    VocabEntry {
        root: ORG.root,
        attributes: ORG.keys,
    },
    VocabEntry {
        root: PROJECT.root,
        attributes: PROJECT.keys,
    },
    VocabEntry {
        root: PLACE.root,
        attributes: PLACE.keys,
    },
    VocabEntry {
        root: AGENT.root,
        attributes: AGENT.keys,
    },
    VocabEntry {
        root: CONTAINER.root,
        attributes: CONTAINER.keys,
    },
    VocabEntry {
        root: DOMAIN.root,
        attributes: DOMAIN.keys,
    },
    VocabEntry {
        root: DATABASE.root,
        attributes: DATABASE.keys,
    },
];

#[must_use]
pub fn vocabulary() -> &'static [VocabEntry] {
    VOCABULARY
}

pub(super) fn vocab_for(type_name: &str) -> &'static Vocab {
    let key = super::resolve::compact(type_name);
    if key.contains("container") {
        return &CONTAINER;
    }
    if key.contains("tool") && !key.contains("host") {
        return &SOFTWARE;
    }
    match type_root(type_name) {
        TypeRoot::Host => &HOST,
        TypeRoot::Hardware => &HARDWARE,
        TypeRoot::Service => &SERVICE,
        TypeRoot::Model => &MODEL,
        TypeRoot::Software => &SOFTWARE,
        TypeRoot::Repo => &REPO,
        TypeRoot::Person => &PERSON,
        TypeRoot::Org => &ORG,
        TypeRoot::Project => &PROJECT,
        TypeRoot::Place => &PLACE,
        TypeRoot::Agent => &AGENT,
        TypeRoot::Domain => &DOMAIN,
        TypeRoot::Database => &DATABASE,
        TypeRoot::Other => &GENERIC,
    }
}
