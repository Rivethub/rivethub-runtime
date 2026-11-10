use std::collections::HashMap;
use std::hash::Hash as _;
use std::hash::Hasher as _;
use std::num::NonZeroUsize;
use std::sync::Arc;

use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::Policy;
use capture::sanitize::Sanitized;
use capture::sanitize::decide::Endpoint;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Review;
use capture::sanitize::decide::Scores;
use capture::sanitize::marks::MarkProblem;
use capture::sanitize::marks::ReportError;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::sanitize_corpus::Case;
use super::sanitize_corpus::Expect;
use super::sanitize_corpus::Position;
use super::sanitize_corpus::at;
use super::sanitize_corpus::bundle_with;
use super::sanitize_corpus::cases;
use super::sanitize_corpus::corpus_dirs;
use super::sanitize_corpus::positions;

type SendError = Box<dyn std::error::Error + Send + Sync>;

const SECRET: &str = concat!("ghp_", "PLANTEDmapZq8RmT2vLx9WabcdefghijklmnoQ");
const ASSIGNMENT: &str = "password=PLANTEDasgZq8RmT2vLx9W";
const PLAIN: &str = "PLANTEDphZq8RmT2vLx9Wq7";

#[derive(serde::Deserialize)]
struct Ask {
    state: String,
}

#[derive(serde::Serialize)]
struct Answer {
    probabilities: [f64; 2],
}

#[derive(serde::Serialize)]
struct Info {
    model: &'static str,
}

async fn always() -> axum::Json<Answer> {
    axum::Json(Answer {
        probabilities: [0.1, 0.9],
    })
}

async fn hashed(axum::Json(ask): axum::Json<Ask>) -> axum::Json<Answer> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ask.state.hash(&mut hasher);
    let yes = f64::from(u8::try_from(hasher.finish() % 100).unwrap_or(0)) / 100.0;
    axum::Json(Answer {
        probabilities: [1.0 - yes, yes],
    })
}

async fn info() -> axum::Json<Info> {
    axum::Json(Info { model: "stub" })
}

fn policy() -> Policy {
    Policy {
        mapping: sanitize::mapping::Mapping::default(),
        key: LocalKey::from_bytes([7; 32]),
    }
}

async fn scored(url: &str, raw: &SessionBundle) -> Result<Scores, SendError> {
    let candidates = sanitize::scrub(raw, &policy())?.candidates();
    let endpoint = Endpoint {
        url: url.to_owned(),
        concurrency: NonZeroUsize::MIN.saturating_add(3),
    };
    Ok(sanitize::decide::score(&endpoint, &candidates.chunks).await?)
}

fn sanitized(
    raw: &SessionBundle,
    scores: Option<&Scores>,
    mode: Mode,
) -> Result<Sanitized, SendError> {
    let policy = policy();
    let mut scrubbed = sanitize::scrub(raw, &policy)?;
    if let Some(scores) = scores {
        let candidates = scrubbed.candidates();
        let review = Review {
            top: NonZeroUsize::MIN.saturating_add(19),
            threshold: 0.3,
            mode,
        };
        scrubbed.flag(candidates, scores, review)?;
    }
    Ok(scrubbed.seal(&policy, at()?)?)
}

const EVERY_SET: [Marks; 9] = [
    Marks::Keep,
    Marks::Redact,
    Marks::MapSecret,
    Marks::MapAssignment,
    Marks::MapShort,
    Marks::MapPartial,
    Marks::MapPlaceholder,
    Marks::MapFlagged,
    Marks::Mixed,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
enum Marks {
    Keep,
    Redact,
    MapSecret,
    MapAssignment,
    MapShort,
    MapPartial,
    MapPlaceholder,
    MapFlagged,
    Mixed,
}

impl Marks {
    const fn refused(self) -> bool {
        matches!(
            self,
            Self::MapSecret | Self::MapAssignment | Self::MapPartial | Self::MapFlagged
        )
    }

    fn mark(self, number: usize, text: &str) -> Option<String> {
        let token = text
            .split_whitespace()
            .next()
            .unwrap_or("x")
            .trim_matches(['"', '\'', '`']);
        Some(match self {
            Self::Keep => "keep".to_owned(),
            Self::Redact => "redact".to_owned(),
            Self::MapSecret => format!("map `{token}` as {SECRET}"),
            Self::MapAssignment => format!("map `{token}` as {ASSIGNMENT}"),
            Self::MapShort => format!("map `{token}` as A"),
            Self::MapPartial => {
                let chars: Vec<char> = text.chars().collect();
                let middle: String = chars
                    .get(chars.len() / 3..(2 * chars.len() / 3).max(chars.len() / 3 + 1))
                    .unwrap_or_default()
                    .iter()
                    .collect();
                let middle = middle.lines().next().unwrap_or_default().trim().to_owned();
                format!("map `{middle}` as [REDACTED]")
            }
            Self::MapPlaceholder => match text.find("[REDACTED").and_then(|start| {
                text.get(start..)
                    .and_then(|rest| rest.find(']'))
                    .and_then(|end| text.get(start..=start + end))
            }) {
                Some(placeholder) => format!("map `{placeholder}` as {PLAIN}"),
                None => format!("map `{token}` as {PLAIN}"),
            },
            Self::MapFlagged => format!("map `{token}` as [FLAGGED:7]"),
            Self::Mixed => match number % 4 {
                0 => "keep".to_owned(),
                1 => "redact".to_owned(),
                2 => format!("map `{token}` as [ORG]"),
                _ => return None,
            },
        })
    }
}

fn set_marks(report: &str, mark: &dyn Fn(usize) -> Option<String>) -> String {
    let mut current = None;
    report
        .split('\n')
        .map(|line| {
            if let Some(rest) = line.strip_prefix("### Flag ") {
                current = rest
                    .split(' ')
                    .next()
                    .and_then(|digits| digits.parse().ok());
            }
            match (line, current.and_then(mark)) {
                ("Mark: open", Some(mark)) => format!("Mark: {mark}"),
                _ => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn leaks(case: &Case, text: &str) -> Vec<String> {
    if case.expect != Expect::Redact {
        return Vec::new();
    }
    case.planted
        .iter()
        .filter(|planted| {
            let escaped = serde_json::to_string(planted).unwrap_or_default();
            text.contains(planted.as_str()) || text.contains(escaped.trim_matches('"'))
        })
        .cloned()
        .collect()
}

fn unresolved(bundle: &SessionBundle) -> Option<u32> {
    match &bundle.manifest.sanitization {
        Sanitization::Sanitized(stamp) => Some(stamp.unresolved_flags),
        Sanitization::Raw => None,
    }
}

fn marked_report(
    report: &str,
    texts: &HashMap<usize, String>,
    marks: Marks,
) -> Result<(String, sanitize::ReportFlags), String> {
    let mut fallback: Vec<usize> = Vec::new();
    loop {
        let edited = set_marks(report, &|number| {
            if fallback.contains(&number) {
                return Some("redact".to_owned());
            }
            marks.mark(number, texts.get(&number).map_or("", String::as_str))
        });
        match sanitize::marks::read_flags(&edited) {
            Ok(flags) => return Ok((edited, flags)),
            Err(ReportError::Mark {
                number,
                problem:
                    MarkProblem::TermMissing(_)
                    | MarkProblem::BlankTerm
                    | MarkProblem::CutsPlaceholder(_)
                    | MarkProblem::PlaceholderForm(_)
                    | MarkProblem::MapShape,
            }) if !fallback.contains(&number) && !marks.refused() => fallback.push(number),
            Err(error) => return Err(error.to_string()),
        }
    }
}

struct Stage<'a> {
    case: &'a Case,
    mode: Mode,
    bundle: &'a SessionBundle,
    plain: &'a SessionBundle,
    report: &'a str,
    texts: &'a HashMap<usize, String>,
    key: &'a LocalKey,
    local: &'a sanitize::LocalReview,
}

impl Stage<'_> {
    fn problems(&self, marks: Marks) -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        let (edited, flags) = match marked_report(self.report, self.texts, marks) {
            Ok(read) if marks.refused() && !self.texts.is_empty() => {
                found.push(("read", "a refused placeholder was accepted".to_owned()));
                read
            }
            Ok(read) => read,
            Err(_) if marks.refused() => return found,
            Err(error) => return vec![("read", error)],
        };
        let resolved = match sanitize::resolve(self.bundle, &flags, self.local, self.key) {
            Ok(resolved) => resolved,
            Err(error) => return vec![("resolve", error.to_string())],
        };
        let after = &resolved.sealed.bundle;
        let text = String::from_utf8_lossy(&resolved.sealed.text).into_owned();
        found.extend(
            sanitize::secrets(after)
                .iter()
                .map(|c| ("ingest scan", c.to_string())),
        );
        found.extend(
            leaks(self.case, &text)
                .into_iter()
                .map(|v| ("bundle leak", v)),
        );
        let open = if marks == Marks::Mixed {
            self.texts.keys().filter(|number| *number % 4 == 3).count()
        } else {
            0
        };
        let withheld = if self.mode == Mode::Block { open } else { 0 };
        let state = (resolved.open, unresolved(after), sanitize::withheld(after));
        if state != (open, u32::try_from(open).ok(), withheld) {
            found.push(("state", format!("{state:?} for {open} unmarked")));
        }
        if marks == Marks::Keep && after.sessions != self.plain.sessions {
            found.push((
                "fidelity",
                "keep-all differs from sanitize without --decide".to_owned(),
            ));
        }
        let applied: Vec<usize> = resolved.applied.iter().map(|(number, _)| *number).collect();
        let rewritten = flags.rewrite(&edited, resolved.open, resolved.total, &applied);
        found.extend(
            leaks(self.case, &rewritten)
                .into_iter()
                .map(|v| ("report leak", v)),
        );
        match sanitize::marks::read_flags(&rewritten)
            .map_err(SendError::from)
            .and_then(|again| {
                Ok((
                    sanitize::resolve(after, &again, self.local, self.key)?,
                    again,
                ))
            }) {
            Ok((twice, again)) => {
                if twice.sealed.text != resolved.sealed.text {
                    found.push((
                        "idempotence",
                        "a second resolve changed the bundle".to_owned(),
                    ));
                }
                if again.rewrite(&rewritten, twice.open, twice.total, &[]) != rewritten {
                    found.push((
                        "idempotence",
                        "a second resolve changed the report".to_owned(),
                    ));
                }
            }
            Err(error) => found.push(("idempotence", error.to_string())),
        }
        found
    }
}

async fn review_stages(
    url: &str,
    sets: &[Marks],
    case: &Case,
    position: Position,
) -> Result<Vec<String>, SendError> {
    let raw = bundle_with(position, &case.raw).map_err(|error| error.to_string())?;
    let plain = sanitized(&raw, None, Mode::Advisory)?;
    let scores = scored(url, &raw).await?;
    let key = LocalKey::from_bytes([7; 32]);
    let mut found = Vec::new();
    for mode in [Mode::Advisory, Mode::Block] {
        let decided = sanitized(&raw, Some(&scores), mode)?;
        let line = |stage: &str, what: &dyn std::fmt::Display| {
            format!("{} | {position:?} | {mode} {stage} | {what}", case.shape)
        };
        let report = decided.report.markdown("s.clean.json", "none");
        let bundle = &decided.sealed.bundle;
        found.extend(
            sanitize::secrets(bundle)
                .iter()
                .map(|c| line("sanitize", c)),
        );
        found.extend(
            leaks(case, &String::from_utf8_lossy(&decided.sealed.text))
                .iter()
                .map(|v| line("bundle leak", v)),
        );
        found.extend(leaks(case, &report).iter().map(|v| line("report leak", v)));
        let flags = sanitize::marks::read_flags(&report)?;
        let texts: HashMap<usize, String> = flags
            .flags
            .iter()
            .map(|flag| (flag.number, flag.text.clone()))
            .collect();
        let stage = Stage {
            case,
            mode,
            bundle,
            plain: &plain.sealed.bundle,
            report: &report,
            texts: &texts,
            key: &key,
            local: decided.local.as_ref().ok_or("no local review")?,
        };
        for &marks in sets {
            found.extend(
                stage
                    .problems(marks)
                    .into_iter()
                    .map(|(at, what)| line(&format!("{marks} {at}"), &what)),
            );
        }
    }
    Ok(found)
}

async fn every_case(
    routes: axum::Router,
    sets: &'static [Marks],
) -> Result<(usize, Vec<String>), SendError> {
    let stub =
        testkit::stub_server(routes.route("/v1/decide/info", axum::routing::get(info))).await?;
    let url = format!("http://{}", stub.addr);
    let slots = Arc::new(tokio::sync::Semaphore::new(
        std::thread::available_parallelism().map_or(4, NonZeroUsize::get),
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for corpus in corpus_dirs()? {
        for case in cases(&corpus).map_err(|error| error.to_string())? {
            let case = Arc::new(case);
            for position in positions(&case.position)? {
                let (corpus, case, url, slots) =
                    (corpus.clone(), case.clone(), url.clone(), slots.clone());
                tasks.spawn(async move {
                    let _slot = slots.acquire_owned().await?;
                    let lines = review_stages(&url, sets, &case, position).await?;
                    Ok::<_, SendError>(
                        lines
                            .into_iter()
                            .map(|line| format!("{corpus} | {line}"))
                            .collect::<Vec<_>>(),
                    )
                });
            }
        }
    }
    let runs = tasks.len();
    let mut found = Vec::new();
    while let Some(lines) = tasks.join_next().await {
        found.extend(lines??);
    }
    found.sort();
    stub.close().await?;
    Ok((runs, found))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_corpus_case_through_the_review_and_every_mark_set_stays_a_bundle_ingest_accepts()
-> Result<(), SendError> {
    for (routes, sets) in [
        (
            axum::Router::new().route("/v1/decide", axum::routing::post(always)),
            &EVERY_SET[..],
        ),
        (
            axum::Router::new().route("/v1/decide", axum::routing::post(hashed)),
            &[Marks::Keep, Marks::Redact, Marks::Mixed][..],
        ),
    ] {
        let (runs, found) = every_case(routes, sets).await?;
        assert!(runs > 0, "no corpus cases ran");
        assert!(
            found.is_empty(),
            "{} problems over {runs} corpus runs, advisory and --block, each mark set resolved twice\ncorpus | shape | position | stage | match\n{}",
            found.len(),
            found.join("\n")
        );
    }
    Ok(())
}
