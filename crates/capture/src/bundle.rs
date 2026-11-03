use thiserror::Error;
use wire::capture::SESSION_BUNDLE_VERSION;
use wire::capture::SessionBundle;

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("session bundle is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session bundle version {found} is not supported")]
    Version { found: u32 },
}

pub fn parse(text: &str) -> Result<SessionBundle, BundleError> {
    let bundle: SessionBundle = serde_json::from_str(text)?;
    if bundle.version == SESSION_BUNDLE_VERSION {
        Ok(bundle)
    } else {
        Err(BundleError::Version {
            found: bundle.version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use wire::capture::SESSION_BUNDLE_VERSION;
    use wire::capture::SessionBundle;

    #[test]
    fn a_current_bundle_parses_and_another_version_does_not() {
        let bundle = SessionBundle {
            version: SESSION_BUNDLE_VERSION,
            sessions: Vec::new(),
        };
        let text = serde_json::to_string(&bundle).expect("bundle json");
        let parsed = parse(&text).expect("current bundle");
        assert_eq!(parsed, bundle);
        let stale = text.replace(
            &SESSION_BUNDLE_VERSION.to_string(),
            &(SESSION_BUNDLE_VERSION + 1).to_string(),
        );
        let error = parse(&stale).expect_err("future bundle");
        assert_eq!(
            error.to_string(),
            format!(
                "session bundle version {} is not supported",
                SESSION_BUNDLE_VERSION + 1
            )
        );
    }
}
