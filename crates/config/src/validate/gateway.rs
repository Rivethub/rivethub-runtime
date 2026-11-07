use crate::enums::{HarnessId, listed};
use crate::model::{GatewaySection, HarnessSlot};
use crate::report::ValidationResult;

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

pub(super) fn validate(out: &mut ValidationResult, section: &GatewaySection) {
    if section
        .host
        .as_deref()
        .is_some_and(|host| host.trim().is_empty())
    {
        out.error(
            "gateway.host",
            "\"gateway.host\" must be a non-empty string (bind address, e.g. 127.0.0.1 or 0.0.0.0)",
        );
    }
    if section.port == 0 {
        out.error(
            "gateway.port",
            "\"gateway.port\" must be an integer between 1 and 65535",
        );
    }
    if let Some(static_dir) = &section.static_dir
        && static_dir.as_os_str().is_empty()
    {
        out.error(
            "gateway.static_dir",
            "\"gateway.static_dir\" must be a non-empty string path",
        );
    }
    for (path, entries) in [
        ("gateway.allowed_origins", &section.allowed_origins),
        ("gateway.allowed_hosts", &section.allowed_hosts),
    ] {
        if entries.iter().any(|entry| entry.trim().is_empty()) {
            out.error(
                path,
                format!("\"{path}\" must be a list of non-empty strings"),
            );
        }
    }
    allowed_harnesses(out, section);
    terminal_roots(out, section);
    exposed_terminal_gate(section, out);
}

fn allowed_harnesses(out: &mut ValidationResult, section: &GatewaySection) {
    let Some(entries) = &section.allowed_harnesses else {
        return;
    };
    let mut blank_entry = false;
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            HarnessSlot::Known(_) => {}
            HarnessSlot::Unknown(id) if id.trim().is_empty() => blank_entry = true,
            HarnessSlot::Unknown(id) => out.warning(
                format!("gateway.allowed_harnesses[{index}]"),
                format!(
                    "Unknown harness id \"{id}\" — expected one of: {}",
                    listed::<HarnessId>()
                ),
            ),
        }
    }
    if blank_entry {
        out.error(
            "gateway.allowed_harnesses",
            "\"gateway.allowed_harnesses\" must be a list of non-empty harness ids (omit the key to allow all)",
        );
    }
}

fn terminal_roots(out: &mut ValidationResult, section: &GatewaySection) {
    let Some(roots) = section
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.roots.as_ref())
    else {
        return;
    };
    if roots.is_empty() || roots.iter().any(|root| !root.is_absolute()) {
        out.error(
            "gateway.terminal.roots",
            "\"gateway.terminal.roots\" must be a non-empty list of absolute paths (omit the key for the defaults)",
        );
    }
}

fn exposed_terminal_gate(section: &GatewaySection, out: &mut ValidationResult) {
    if !matches!(section.enabled, Some(true)) {
        return;
    }
    let Some(terminal) = &section.terminal else {
        return;
    };
    if !matches!(terminal.enabled, Some(true)) {
        return;
    }
    if matches!(terminal.open, Some(true)) {
        return;
    }
    let certified = section
        .tls_cert
        .as_deref()
        .is_some_and(|cert| !cert.is_empty());
    if certified {
        return;
    }
    let loopback = section.host.as_deref().is_none_or(is_loopback);
    if loopback {
        return;
    }
    out.error(
        "gateway.tls_cert",
        "\"gateway.tls_cert\" (and gateway.tls_key) is required when gateway.terminal.enabled is true and gateway.host is not loopback (127.0.0.1/::1/localhost) — an exposed terminal without TLS would hang an unauthenticated shell on the network. Set gateway.tls_cert/tls_key, bind gateway.host to loopback, disable terminals, or set gateway.terminal.open: true on a trusted private network.",
    );
}
