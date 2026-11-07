use crate::model::GatewaySection;
use crate::report::ValidationResult;

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
}
