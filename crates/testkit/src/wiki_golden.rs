use std::fmt::Debug;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    Rejected,
    Differs,
}

#[derive(Debug)]
pub struct Skip {
    pub function: &'static str,
    pub kind: SkipKind,
    pub reason: &'static str,
    pub lines: &'static [usize],
}

#[derive(Debug)]
pub struct Corpus {
    dir: PathBuf,
    skips: &'static [Skip],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ThrownName {
    WikiParseError,
    #[serde(rename = "YAMLParseError")]
    YamlParseError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thrown {
    pub name: ThrownName,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[serde(rename = "fn")]
    function: String,
    #[serde(rename = "in")]
    input: Value,
    out: Option<Value>,
    throws: Option<Thrown>,
}

enum Expected<O> {
    Value(O),
    Thrown(Thrown),
}

fn decode_recording(value: Value) -> Value {
    match value {
        Value::Object(members) => {
            if members.len() == 1 && members.get("$t") == Some(&Value::from("undefined")) {
                return Value::Null;
            }
            Value::Object(
                members
                    .into_iter()
                    .map(|(key, member)| (key, decode_recording(member)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(decode_recording).collect()),
        scalar @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => scalar,
    }
}

#[derive(Debug, Default)]
pub struct Tally {
    pub passed: usize,
    pub skipped: usize,
}

impl Corpus {
    #[must_use]
    pub fn new(dir: PathBuf, skips: &'static [Skip]) -> Self {
        Self { dir, skips }
    }

    fn skip_for(&self, function: &str, line: usize) -> Option<&'static Skip> {
        self.skips
            .iter()
            .find(|skip| skip.function == function && skip.lines.contains(&line))
    }

    pub fn replay<A, O, E, F, M>(
        &self,
        function: &str,
        run: F,
        matches_thrown: M,
    ) -> Result<Tally, String>
    where
        A: DeserializeOwned,
        O: DeserializeOwned + PartialEq + Debug,
        E: Debug,
        F: Fn(A) -> Result<O, E>,
        M: Fn(&E, &Thrown) -> bool,
    {
        replay(self, function, run, matches_thrown)
    }
}

fn replay<A, O, E, F, M>(
    corpus: &Corpus,
    function: &str,
    run: F,
    matches_thrown: M,
) -> Result<Tally, String>
where
    A: DeserializeOwned,
    O: DeserializeOwned + PartialEq + Debug,
    E: Debug,
    F: Fn(A) -> Result<O, E>,
    M: Fn(&E, &Thrown) -> bool,
{
    let path = corpus.dir.join(format!("{function}.jsonl"));
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut tally = Tally::default();
    let mut failures = Vec::new();
    let mut failing_lines = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let case: Case =
            serde_json::from_str(raw).map_err(|error| format!("line {line}: {error}"))?;
        assert_eq!(case.function, function, "line {line}");
        let skip = corpus.skip_for(function, line);
        let outcome = run_case(&case, &run, &matches_thrown);
        match (skip, outcome) {
            (None, Outcome::Pass) => tally.passed += 1,
            (Some(skip), Outcome::Rejected(_)) if skip.kind == SkipKind::Rejected => {
                tally.skipped += 1;
            }
            (Some(skip), Outcome::Fail(_)) if skip.kind == SkipKind::Differs => tally.skipped += 1,
            (Some(skip), outcome) => failures.push(format!(
                "line {line}: listed as {:?} ({}) but {}",
                skip.kind,
                skip.reason,
                outcome.describe()
            )),
            (None, outcome) => {
                failing_lines.push(line);
                failures.push(format!("line {line}: {}", outcome.describe()));
            }
        }
    }
    if failures.is_empty() {
        Ok(tally)
    } else {
        let count = failures.len();
        failures.truncate(400);
        Err(format!(
            "{function}: {count} failing case(s)\n{}\nunlisted failing lines: {failing_lines:?}",
            failures.join("\n\n")
        ))
    }
}

enum Outcome {
    Pass,
    Rejected(String),
    Fail(String),
}

impl Outcome {
    fn describe(&self) -> String {
        match self {
            Self::Pass => "passes".to_owned(),
            Self::Rejected(error) => format!("input rejected: {error}"),
            Self::Fail(detail) => detail.clone(),
        }
    }
}

fn run_case<A, O, E, F, M>(case: &Case, run: &F, matches_thrown: &M) -> Outcome
where
    A: DeserializeOwned,
    O: DeserializeOwned + PartialEq + Debug,
    E: Debug,
    F: Fn(A) -> Result<O, E>,
    M: Fn(&E, &Thrown) -> bool,
{
    let input = match serde_json::from_value::<A>(decode_recording(case.input.clone())) {
        Ok(input) => input,
        Err(error) => return Outcome::Rejected(error.to_string()),
    };
    let expected = match (&case.out, &case.throws) {
        (_, Some(thrown)) => Expected::Thrown(thrown.clone()),
        (Some(out), None) => match serde_json::from_value::<O>(decode_recording(out.clone())) {
            Ok(out) => Expected::Value(out),
            Err(error) => return Outcome::Fail(format!("recorded output does not parse: {error}")),
        },
        (None, None) => match serde_json::from_value::<O>(Value::Null) {
            Ok(out) => Expected::Value(out),
            Err(error) => return Outcome::Fail(format!("recorded output missing: {error}")),
        },
    };
    match (run(input), expected) {
        (Ok(actual), Expected::Value(expected)) if actual == expected => Outcome::Pass,
        (Ok(actual), Expected::Value(expected)) => {
            Outcome::Fail(difference(&format!("{expected:?}"), &format!("{actual:?}")))
        }
        (Err(error), Expected::Thrown(thrown)) if matches_thrown(&error, &thrown) => Outcome::Pass,
        (Err(error), Expected::Thrown(thrown)) => {
            Outcome::Fail(format!("expected thrown {thrown:?}, got error {error:?}"))
        }
        (Ok(actual), Expected::Thrown(thrown)) => Outcome::Fail(format!(
            "expected thrown {thrown:?}, got {}",
            clip(&format!("{actual:?}"))
        )),
        (Err(error), Expected::Value(expected)) => Outcome::Fail(format!(
            "expected {}, got error {error:?}",
            clip(&format!("{expected:?}"))
        )),
    }
}

pub const fn never_thrown<E>(_: &E, _: &Thrown) -> bool {
    false
}

fn difference(expected: &str, actual: &str) -> String {
    let expected: Vec<char> = expected.chars().collect();
    let actual: Vec<char> = actual.chars().collect();
    let first = expected
        .iter()
        .zip(&actual)
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| expected.len().min(actual.len()));
    let start = first.saturating_sub(120);
    let window = |text: &[char]| text.iter().skip(start).take(240).collect::<String>();
    format!(
        "differs at {first}\n  expected …{}…\n  actual   …{}…",
        window(&expected),
        window(&actual)
    )
}

fn clip(text: &str) -> String {
    text.chars().take(240).collect()
}
