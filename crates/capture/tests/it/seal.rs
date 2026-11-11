use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::Policy;
use capture::sanitize::SessionRules;
use capture::seal::CodeError;
use capture::seal::OpenError;
use capture::seal::SealError;
use capture::seal::ShareCode;
use capture::seal::ShareKey;
use capture::sign::NodeKey;
use capture::sign::Trust;
use capture::sign::TrustStore;
use capture::sign::Verification;
use chrono::DateTime;
use chrono::Utc;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Sanitization;
use wire::capture::SessionOrigin;
use wire::message::Role;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DROP_ID: &str = "AAAAAAAAAAAAAAAAAAAAAA";

const fn at() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

fn session(content: &str) -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            created_at: Some(at()),
            updated_at: Some(at()),
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            channel: None,
            title: Some("set up AcmeKit".to_owned()),
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![CaptureMessage {
                event_id: "evt-1".to_owned(),
                role: Role::User,
                content: content.to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: Some(at()),
            }],
        },
    }
}

fn author() -> BundleAuthor {
    BundleAuthor {
        user: "phil".to_owned(),
        node: "desk".to_owned(),
    }
}

fn sanitized(content: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let raw =
        capture::bundle::seal(author(), at(), Sanitization::Raw, vec![session(content)])?.bundle;
    let policy = Policy {
        mapping: sanitize::mapping::Mapping::default(),
        key: LocalKey::from_bytes([7; 32]),
        sessions: SessionRules::default(),
    };
    Ok(sanitize::run(&raw, &policy, at())?.sealed.text)
}

fn node_key(directory: &tempfile::TempDir) -> Result<NodeKey, capture::sign::SignError> {
    NodeKey::load_or_create(&NodeKey::path_in(directory.path()))
}

fn code(key: &ShareKey) -> String {
    format!("rhb1:{DROP_ID}#{}", key.reveal())
}

#[test]
fn round_trips_in_memory() -> TestResult {
    let text = sanitized("deploy AcmeKit")?;
    let sealed = capture::seal::seal(&text, None, 0)?;
    assert!(sealed.ciphertext.starts_with(b"age-encryption.org/v1\n"));
    assert!(
        !sealed
            .ciphertext
            .windows(b"AcmeKit".len())
            .any(|window| window == b"AcmeKit"),
        "the ciphertext carries no plaintext"
    );
    assert_eq!(sealed.titles, ["set up AcmeKit"]);
    assert_eq!(sealed.author, "phil");
    let parsed: ShareCode = code(&sealed.key).parse()?;
    assert_eq!(parsed.id.as_str(), DROP_ID);
    let opened = capture::seal::open(sealed.ciphertext, &parsed.key, &TrustStore::default())?;
    assert_eq!(
        opened.bundle,
        capture::bundle::parse(std::str::from_utf8(&text)?)?
    );
    assert_eq!(opened.verification, Verification::Unsigned);
    Ok(())
}

#[test]
fn a_raw_bundle_is_refused() {
    let raw = capture::bundle::seal(author(), at(), Sanitization::Raw, vec![session("x")])
        .expect("raw")
        .text;
    assert!(matches!(
        capture::seal::seal(&raw, None, 0),
        Err(SealError::Raw)
    ));
}

#[test]
fn a_typo_in_the_key_fails_the_checksum() -> TestResult {
    let key = ShareKey::generate();
    let good = code(&key);
    good.parse::<ShareCode>()?;
    let last = good.chars().last().ok_or("code")?;
    let swapped = if last == 'q' { 'p' } else { 'q' };
    let typo = format!("{}{swapped}", &good[..good.len() - 1]);
    assert_eq!(typo.parse::<ShareCode>().err(), Some(CodeError::Checksum));
    assert_eq!(
        format!("{good}x").parse::<ShareCode>().err(),
        Some(CodeError::Checksum)
    );
    assert_eq!(
        good.replace("rhb1:", "").parse::<ShareCode>().err(),
        Some(CodeError::Prefix)
    );
    assert_eq!(
        format!("rhb1:{DROP_ID}").parse::<ShareCode>().err(),
        Some(CodeError::NoKey)
    );
    assert_eq!(
        good.replace(DROP_ID, "short").parse::<ShareCode>().err(),
        Some(CodeError::Id)
    );
    let shouted = format!("rhb1:{DROP_ID}#{}", key.reveal().to_uppercase());
    assert_eq!(shouted.parse::<ShareCode>()?.key.reveal(), key.reveal());
    Ok(())
}

#[test]
fn a_flipped_ciphertext_byte_fails() -> TestResult {
    let sealed = capture::seal::seal(&sanitized("deploy")?, None, 0)?;
    let trust = TrustStore::default();
    for index in [sealed.ciphertext.len() / 2, sealed.ciphertext.len() - 1] {
        let mut flipped = sealed.ciphertext.clone();
        *flipped.get_mut(index).ok_or("index")? ^= 0x01;
        assert!(matches!(
            capture::seal::open(flipped, &sealed.key, &trust),
            Err(OpenError::Decrypt)
        ));
    }
    assert!(matches!(
        capture::seal::open(sealed.ciphertext, &ShareKey::generate(), &trust),
        Err(OpenError::Decrypt)
    ));
    Ok(())
}

#[test]
fn decompression_is_capped() -> TestResult {
    let zeros = vec![0u8; capture::seal::MAX_PLAIN_BYTES + 1];
    let mut plaintext = b"RHB1\n{}\n".to_vec();
    plaintext.extend_from_slice(&zstd::bulk::compress(&zeros, 19)?);
    let key = ShareKey::generate();
    let full: ShareCode = code(&key).parse()?;
    let ciphertext = age::encrypt(&recipient(&key)?, &plaintext)?;
    assert!(ciphertext.len() < 1 << 20, "a small download");
    assert!(matches!(
        capture::seal::open(ciphertext, &full.key, &TrustStore::default()),
        Err(OpenError::TooLarge)
    ));
    Ok(())
}

fn recipient(key: &ShareKey) -> Result<age::x25519::Recipient, &'static str> {
    Ok(format!("AGE-SECRET-KEY-1{}", key.reveal().to_uppercase())
        .parse::<age::x25519::Identity>()?
        .to_public())
}

#[test]
fn signature_states_map_to_verification() -> TestResult {
    let directory = tempfile::tempdir()?;
    let signer = node_key(&directory)?;
    let text = sanitized("deploy")?;
    let sealed = capture::seal::seal(&text, Some(&signer), 0)?;
    assert_eq!(sealed.signed, Some(signer.public_key()));
    let unsigned = capture::seal::seal(&text, None, 0)?;
    let mut trust = TrustStore::default();
    let open = |sealed: &capture::seal::Sealed, trust: &TrustStore| {
        capture::seal::open(sealed.ciphertext.clone(), &sealed.key, trust)
            .map(|opened| opened.verification)
    };
    assert_eq!(open(&unsigned, &trust)?, Verification::Unsigned);
    assert_eq!(
        open(&sealed, &trust)?,
        Verification::Signed {
            key: signer.public_key(),
            trust: Trust::NotListed {
                trusted_as: Vec::new()
            },
        }
    );
    trust.add("alex", signer.public_key())?;
    let wrong_name = open(&sealed, &trust)?;
    assert_eq!(
        wrong_name,
        Verification::Signed {
            key: signer.public_key(),
            trust: Trust::NotListed {
                trusted_as: vec!["alex".to_owned()]
            },
        }
    );
    assert!(
        wrong_name
            .reason("phil")
            .contains("you trust as alex, not as phil")
    );
    trust.add("phil", signer.public_key())?;
    assert_eq!(
        open(&sealed, &trust)?,
        Verification::Signed {
            key: signer.public_key(),
            trust: Trust::Listed,
        }
    );
    assert_eq!(
        open(&sealed, &trust)?.kind(),
        wire::capture::ReceiptVerification::Verified
    );
    Ok(())
}

#[test]
fn a_bad_signature_refuses() -> TestResult {
    let directory = tempfile::tempdir()?;
    let signer = node_key(&directory)?;
    let text = sanitized("deploy")?;
    let signature = signer.sign(&text, "phil");
    let mut forged = signature.clone();
    let mut bytes = base64_decode(&forged.sig)?;
    *bytes.first_mut().ok_or("signature bytes")? ^= 0x01;
    forged.sig = base64_encode(&bytes);
    let mut trust = TrustStore::default();
    trust.add("phil", signer.public_key())?;
    assert!(trust.verification(Some(&signature), &text, "phil").is_ok());
    assert!(trust.verification(Some(&forged), &text, "phil").is_err());
    assert!(
        trust
            .verification(Some(&signature), &text, "mallory")
            .is_err(),
        "the exporter name is covered by the signature"
    );
    let mut plaintext = b"RHB1\n".to_vec();
    plaintext.extend_from_slice(&serde_json::to_vec(&Envelope { signature: &forged })?);
    plaintext.push(b'\n');
    plaintext.extend_from_slice(&zstd::bulk::compress(&text, 3)?);
    let key = ShareKey::generate();
    let ciphertext = age::encrypt(&recipient(&key)?, &plaintext)?;
    assert!(matches!(
        capture::seal::open(ciphertext, &key, &trust),
        Err(OpenError::Signature(_))
    ));
    Ok(())
}

#[derive(serde::Serialize)]
struct Envelope<'a> {
    signature: &'a capture::sign::Signature,
}

fn base64_decode(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(text)
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn the_share_code_key_is_redacted_where_text_is_captured() -> TestResult {
    let key = ShareKey::generate();
    let line = format!("send this privately: {}", code(&key));
    let redacted = capture::redaction::Redaction::builtins().redact_text(&line);
    assert_eq!(
        redacted,
        format!("send this privately: rhb1:{DROP_ID}#[REDACTED:rivethub_share_code]")
    );
    let sanitized = String::from_utf8(sanitized(&line)?)?;
    assert!(!sanitized.contains(&key.reveal()));
    assert!(sanitized.contains("[REDACTED:rivethub_share_code]"));
    Ok(())
}

#[test]
fn trust_entries_round_trip_through_the_file() -> TestResult {
    let directory = tempfile::tempdir()?;
    let signer = node_key(&directory)?;
    let path = TrustStore::path_in(directory.path());
    let mut trust = TrustStore::load(&path)?;
    trust.add("phil", signer.public_key())?;
    assert!(trust.add("phil", signer.public_key()).is_err());
    trust.save(&path)?;
    let again = TrustStore::load(&path)?;
    assert_eq!(again, trust);
    assert!(std::fs::read_to_string(&path)?.contains("[[sender]]"));
    let mut again = again;
    assert_eq!(again.remove("phil", None), 1);
    assert_eq!(again.senders().len(), 0);
    let shown = signer.public_key().to_string();
    assert_eq!(
        shown.parse::<capture::sign::PublicKey>()?,
        signer.public_key()
    );
    assert!(signer.public_key().fingerprint().starts_with("ed25519:"));
    Ok(())
}
