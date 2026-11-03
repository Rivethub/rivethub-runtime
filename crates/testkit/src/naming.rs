#[must_use]
pub fn translate_recorded_product_names(text: &str) -> String {
    let text = match text.strip_prefix("den-") {
        Some(suffix) => format!("rivethub-{suffix}"),
        None => text.to_owned(),
    };
    text.replace("~/.rivetos", "~/.rivethub")
        .replace("RivetOS", "RivetHub")
        .replace("rivetos", "rh")
        .replace("[den-server]", "[rivethub-gateway]")
        .replace("RIVETOS_DEN_TLS_CERT", "RIVETHUB_TLS_CERT")
        .replace("RIVETOS_DEN_TLS_KEY", "RIVETHUB_TLS_KEY")
        .replace("RIVETOS_SESSION_KEY", "RIVETHUB_SESSION_KEY")
        .replace("RIVET_DEN_", "RIVETHUB_")
        .replace("bind den.host", "bind the gateway host")
        .replace(
            "add it to den.allowed_hosts",
            "add it to the gateway allowed_hosts setting",
        )
        .replace(
            "add it to den.allowed_origins",
            "add it to the gateway allowed_origins setting",
        )
        .replace("(den pins", "(RivetHub pins")
        .replace("rivet-den-", "rivethub-")
        .replace(
            "to the node cert from rivet-ca issue-node; enroll clients with rivet-ca issue-client",
            "to the node's holder certificate, ~/.rivethub/tls/holder.cert.pem and holder.key.pem from rh mesh enroll; devices enroll with rh cert renew",
        )
}
