use super::{IgnoredPath, Segment};
use crate::report::{Finding, ValidationResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retired {
    pub path: &'static str,
    pub keeps: &'static [&'static str],
    pub why: &'static str,
}

const CERTIFICATES: &str = "rh no longer issues or checks client certificates; callers off this machine send a bearer token from the users file. Delete it";

pub const RETIRED: &[Retired] = &[
    Retired {
        path: "runtime.profile",
        keeps: &[],
        why: "every rh node serves the memory routes only, so there is no profile to choose. Delete it",
    },
    Retired {
        path: "mesh",
        keeps: &["node_name"],
        why: "rh has no mesh; peer discovery, heartbeats and the agent channel were removed, and only mesh.node_name is still read, as this node's name. Delete the other mesh keys",
    },
    Retired {
        path: "gateway.certificate_authority",
        keeps: &[],
        why: CERTIFICATES,
    },
    Retired {
        path: "gateway.devices",
        keeps: &[],
        why: CERTIFICATES,
    },
    Retired {
        path: "gateway.tls_ca",
        keeps: &[],
        why: CERTIFICATES,
    },
    Retired {
        path: "gateway.tls_crl",
        keeps: &[],
        why: CERTIFICATES,
    },
    Retired {
        path: "gateway.tls_require_client",
        keeps: &[],
        why: CERTIFICATES,
    },
    Retired {
        path: "memory.capture.gateway.cert",
        keeps: &[],
        why: "the capture gateway is reached with memory.capture.gateway.token, not a client certificate. Delete it",
    },
    Retired {
        path: "memory.capture.gateway.key",
        keeps: &[],
        why: "the capture gateway is reached with memory.capture.gateway.token, not a client certificate. Delete it",
    },
];

impl Retired {
    #[must_use]
    pub fn message(&self) -> String {
        format!("\"{}\" is no longer used: {}.", self.path, self.why)
    }

    fn covers(&self, segments: &[Segment]) -> bool {
        let mut wanted = self.path.split('.');
        let mut given = segments.iter();
        loop {
            match (wanted.next(), given.next()) {
                (None, Some(Segment::Key(key))) => return !self.keeps.contains(&key.as_str()),
                (None, _) => return true,
                (Some(part), Some(Segment::Key(key))) if key == part => {}
                (Some(_), _) => return false,
            }
        }
    }
}

pub(super) fn findings(ignored: &[IgnoredPath], out: &mut ValidationResult) -> Vec<IgnoredPath> {
    let mut rest = Vec::new();
    let mut seen: Vec<&Retired> = Vec::new();
    for path in ignored {
        match RETIRED.iter().find(|retired| retired.covers(&path.0)) {
            Some(retired) => {
                if !seen.contains(&retired) {
                    seen.push(retired);
                    out.push(Finding::warning(retired.path, retired.message()));
                }
            }
            None => rest.push(path.clone()),
        }
    }
    rest
}
