use std::io::Read as _;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use base64::Engine as _;
use base64::engine::DecodePaddingMode;
use base64::engine::GeneralPurpose;
use base64::engine::GeneralPurposeConfig;

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const MESSAGE_DECODE_BYTES: u64 = 8 << 20;
const BUNDLE_DECODE_BYTES: u64 = 64 << 20;
const MESSAGE_WORK: u64 = 32 << 20;
const BUNDLE_WORK: u64 = 256 << 20;
pub(super) const CANDIDATE_WORK: u64 = 32;
pub(super) const SCAN_WORK: u64 = 512;
pub(super) const SCANNED_BYTE_WORK: u64 = 2;
const INFLATE_CHUNK: u64 = 64 << 10;
const DER_SEQUENCE: u8 = 0x30;
const DER_INTEGER: u8 = 0x02;
const UTF8_BOM: char = '\u{feff}';

pub(super) enum Decoded {
    Text(String),
    DerPrivateKey,
    OverBudget,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spent {
    pub bytes: u64,
    pub work: u64,
}

impl std::ops::AddAssign for Spent {
    fn add_assign(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.work += other.work;
    }
}

#[derive(Debug)]
pub struct Pool {
    bytes: AtomicU64,
    work: AtomicU64,
}

impl Pool {
    #[must_use]
    pub const fn bundle(workers: usize) -> Self {
        Self {
            bytes: AtomicU64::new(BUNDLE_DECODE_BYTES),
            work: AtomicU64::new(BUNDLE_WORK + workers as u64 * MESSAGE_WORK),
        }
    }

    fn draw(left: &AtomicU64, amount: u64) -> bool {
        left.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
            left.checked_sub(amount)
        })
        .is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Message,
    Bundle,
}

#[derive(Debug, Clone, Copy)]
pub struct Budget<'p> {
    message: u64,
    bundle: u64,
    message_work: u64,
    work: u64,
    pool: Option<&'p Pool>,
    starved: bool,
    halt: Option<Halt>,
}

impl<'p> Budget<'p> {
    #[must_use]
    pub const fn fresh() -> Self {
        Self::after(Spent { bytes: 0, work: 0 })
    }

    #[must_use]
    pub const fn after(spent: Spent) -> Self {
        Self {
            message: MESSAGE_DECODE_BYTES,
            bundle: BUNDLE_DECODE_BYTES.saturating_sub(spent.bytes),
            message_work: MESSAGE_WORK,
            work: BUNDLE_WORK.saturating_sub(spent.work),
            pool: None,
            starved: false,
            halt: None,
        }
    }

    #[must_use]
    pub fn drawing(pool: &'p Pool) -> Self {
        Self {
            pool: Some(pool),
            ..Self::fresh()
        }
    }

    #[must_use]
    pub const fn with_work(self, bundle: u64) -> Self {
        Self {
            work: bundle,
            ..self
        }
    }

    #[must_use]
    pub const fn starved(&self) -> bool {
        self.starved
    }

    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.halt.is_some() || self.starved
    }

    #[must_use]
    pub const fn spent(&self) -> Spent {
        Spent {
            bytes: BUNDLE_DECODE_BYTES - self.bundle,
            work: BUNDLE_WORK - self.work,
        }
    }

    #[must_use]
    pub const fn leaves_room(spent: Spent, next: Spent) -> bool {
        spent.bytes + next.bytes + MESSAGE_DECODE_BYTES <= BUNDLE_DECODE_BYTES
            && spent.work + next.work + MESSAGE_WORK <= BUNDLE_WORK
    }

    pub(super) fn charge(&mut self, work: u64) -> bool {
        if self.exhausted() {
            return false;
        }
        if work > self.work {
            self.halt = Some(Halt::Bundle);
            return false;
        }
        if work > self.message_work {
            self.halt = Some(Halt::Message);
            return false;
        }
        if let Some(pool) = self.pool
            && !Pool::draw(&pool.work, work)
        {
            self.starved = true;
            return false;
        }
        self.message_work -= work;
        self.work -= work;
        true
    }

    pub fn next_message(&mut self) {
        self.message = MESSAGE_DECODE_BYTES;
        self.message_work = MESSAGE_WORK;
        if self.halt == Some(Halt::Message) {
            self.halt = None;
        }
    }

    fn fits(&mut self, bytes: usize) -> bool {
        let fits = bytes as u64 <= self.left();
        if !fits {
            self.take(bytes);
        }
        fits
    }

    const fn left(&self) -> u64 {
        if self.starved {
            0
        } else if self.message < self.bundle {
            self.message
        } else {
            self.bundle
        }
    }

    fn take(&mut self, bytes: usize) -> bool {
        let left = self.left();
        let bytes = bytes as u64;
        let fits = bytes <= left;
        let spent = if fits { bytes } else { left };
        if let Some(pool) = self.pool
            && !Pool::draw(&pool.bytes, spent)
        {
            self.starved = true;
            return false;
        }
        self.message -= spent;
        self.bundle -= spent;
        fits && self.charge(spent)
    }
}

pub(super) fn decoded(blob: &str, budget: &mut Budget<'_>) -> Option<Decoded> {
    let lines: Vec<&str> = blob.lines().map(str::trim).collect();
    let (&first, rest) = lines.split_first()?;
    let width = first.len();
    let mut body = vec![first];
    for line in rest {
        if line.len() == width {
            body.push(line);
        } else {
            if line.len() < width {
                body.push(line);
            }
            break;
        }
    }
    decoded_joined(&body.concat(), budget).or_else(|| {
        body.pop();
        (!body.is_empty())
            .then(|| decoded_joined(&body.concat(), budget))
            .flatten()
    })
}

fn decoded_joined(joined: &str, budget: &mut Budget<'_>) -> Option<Decoded> {
    if !budget.fits(joined.len() / 4 * 3) {
        return Some(Decoded::OverBudget);
    }
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let standard = GeneralPurpose::new(&base64::alphabet::STANDARD, config).decode(joined);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): text that is not base64 in either alphabet is not a blob to scan; only_text_and_der_keys_decode pins it"
        )
    )]
    let bytes = match standard {
        Ok(bytes) => bytes,
        Err(_) => match GeneralPurpose::new(&base64::alphabet::URL_SAFE, config).decode(joined) {
            Ok(bytes) => bytes,
            Err(_) => return None,
        },
    };
    if !budget.take(bytes.len()) {
        return Some(Decoded::OverBudget);
    }
    let bytes = if bytes.starts_with(&GZIP_MAGIC) {
        let mut decoder = flate2::read::GzDecoder::new(bytes.as_slice());
        let mut inflated = Vec::new();
        loop {
            let before = inflated.len();
            if (&mut decoder)
                .take(INFLATE_CHUNK.min(budget.left().saturating_add(1)))
                .read_to_end(&mut inflated)
                .is_err()
            {
                return None;
            }
            let read = inflated.len() - before;
            if read == 0 {
                break;
            }
            if !budget.take(read) {
                return Some(Decoded::OverBudget);
            }
        }
        inflated
    } else {
        bytes
    };
    if is_der_private_key(&bytes) {
        return Some(Decoded::DerPrivateKey);
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): a blob that decodes to bytes that are not UTF-8 is not text to scan; only_text_and_der_keys_decode pins it"
        )
    )]
    let Ok(text) = String::from_utf8(bytes) else {
        return None;
    };
    let text = text.trim_start_matches(UTF8_BOM).trim_start().to_owned();
    (!text.is_empty()).then_some(Decoded::Text(text))
}

fn is_der_private_key(bytes: &[u8]) -> bool {
    let Some((&tag, rest)) = bytes.split_first() else {
        return false;
    };
    if tag != DER_SEQUENCE {
        return false;
    }
    let Some((&length, rest)) = rest.split_first() else {
        return false;
    };
    let header = if length & 0x80 == 0 {
        0
    } else {
        usize::from(length & 0x7f)
    };
    let Some(body) = rest.get(header..) else {
        return false;
    };
    matches!(body, [DER_INTEGER, 1, 0 | 1, ..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_text_and_der_keys_decode() {
        let json = base64::engine::general_purpose::STANDARD.encode(r#"{"a": 1}"#);
        assert!(
            matches!(decoded(&json, &mut Budget::fresh()), Some(Decoded::Text(text)) if text.starts_with('{'))
        );
        let wrapped = format!("{}\n{}", &json[..6], &json[6..]);
        assert!(matches!(
            decoded(&wrapped, &mut Budget::fresh()),
            Some(Decoded::Text(_))
        ));
        let env = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("A=b\n");
        assert!(
            matches!(decoded(&env, &mut Budget::fresh()), Some(Decoded::Text(text)) if text == "A=b\n")
        );
        let noise = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0xfe, 0xfd, 0x00]);
        assert!(decoded(&noise, &mut Budget::fresh()).is_none());
        assert!(decoded("not base64 at all!", &mut Budget::fresh()).is_none());
        let pkcs8 = base64::engine::general_purpose::STANDARD
            .encode([0x30, 0x82, 0x04, 0xbd, 0x02, 0x01, 0x00, 0x30]);
        assert!(matches!(
            decoded(&pkcs8, &mut Budget::fresh()),
            Some(Decoded::DerPrivateKey)
        ));
        let certificate = base64::engine::general_purpose::STANDARD
            .encode([0x30, 0x82, 0x03, 0x5d, 0x30, 0x82, 0x02, 0x45]);
        assert!(decoded(&certificate, &mut Budget::fresh()).is_none());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, b"token=x").expect("write");
        let gz = base64::engine::general_purpose::STANDARD.encode(gz.finish().expect("finish"));
        assert!(
            matches!(decoded(&gz, &mut Budget::fresh()), Some(Decoded::Text(text)) if text == "token=x")
        );
    }

    fn gzip_base64(bytes: &[u8]) -> String {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut gz, bytes).expect("write");
        base64::engine::general_purpose::STANDARD.encode(gz.finish().expect("finish"))
    }

    #[test]
    fn decoding_stops_at_the_message_budget() {
        let blob = gzip_base64(&vec![b'A'; 5 << 20]);
        let mut budget = Budget::fresh();
        assert!(matches!(
            decoded(&blob, &mut budget),
            Some(Decoded::Text(_))
        ));
        assert!(matches!(
            decoded(&blob, &mut budget),
            Some(Decoded::OverBudget)
        ));
        budget.next_message();
        assert!(matches!(
            decoded(&blob, &mut budget),
            Some(Decoded::Text(_))
        ));
    }

    #[test]
    fn decoding_stops_at_the_bundle_budget() {
        let blob = gzip_base64(&vec![b'A'; 5 << 20]);
        let mut budget = Budget::fresh();
        let mut decoded_messages = 0;
        while matches!(decoded(&blob, &mut budget), Some(Decoded::Text(_))) {
            decoded_messages += 1;
            budget.next_message();
        }
        assert_eq!(decoded_messages, BUNDLE_DECODE_BYTES / (5 << 20));
    }

    #[test]
    fn budgets_drawing_on_one_pool_decode_no_more_than_the_bundle_budget() {
        let blob = gzip_base64(&vec![b'A'; 5 << 20]);
        let pool = Pool::bundle(1);
        let mut decoded_messages = 0;
        loop {
            let mut budget = Budget::drawing(&pool);
            if !matches!(decoded(&blob, &mut budget), Some(Decoded::Text(_))) {
                assert!(budget.exhausted());
                break;
            }
            decoded_messages += 1;
        }
        assert_eq!(decoded_messages, BUNDLE_DECODE_BYTES / (5 << 20));
    }

    #[test]
    fn work_stops_at_the_bundle_allowance_and_stays_stopped() {
        let mut budget = Budget::fresh().with_work(100);
        assert!(budget.charge(60));
        assert!(!budget.charge(60));
        assert!(budget.exhausted());
        budget.next_message();
        assert!(
            budget.exhausted(),
            "a spent bundle holds every later message"
        );
        assert!(!budget.charge(1));
    }

    #[test]
    fn a_message_that_spends_its_work_is_held_alone() {
        let mut budget = Budget::fresh();
        assert!(budget.charge(MESSAGE_WORK));
        assert!(!budget.charge(1));
        assert!(budget.exhausted());
        budget.next_message();
        assert!(!budget.exhausted());
        assert!(budget.charge(1));
    }

    #[test]
    fn budgets_drawing_on_one_pool_spend_no_more_than_the_bundle_work_and_a_message_per_worker() {
        let pool = Pool::bundle(3);
        let mut charged = 0;
        loop {
            let mut budget = Budget::drawing(&pool);
            if !budget.charge(MESSAGE_WORK) {
                assert!(budget.exhausted());
                break;
            }
            charged += MESSAGE_WORK;
        }
        assert_eq!(charged, BUNDLE_WORK + 3 * MESSAGE_WORK);
    }

    #[test]
    fn a_unit_is_accepted_only_while_a_whole_message_of_work_is_left() {
        let spent = Spent {
            bytes: 0,
            work: BUNDLE_WORK - 2 * MESSAGE_WORK,
        };
        let next = Spent {
            bytes: 0,
            work: MESSAGE_WORK,
        };
        assert!(Budget::leaves_room(spent, next));
        assert!(!Budget::leaves_room(
            Spent {
                work: spent.work + 1,
                ..spent
            },
            next
        ));
    }
}
