use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use capture::sanitize;
use capture::sanitize::LocalKey;
use capture::sanitize::ReportFlags;
use capture::sanitize::decide::Mode;
use capture::share;
use capture::share::Decider;
use capture::share::Outcome;
use config::MappingFile;
use kernel::memory::share::desk::FlagChoice;
use kernel::memory::share::desk::Mark;

use crate::sanitize_decide::RISKY;
use crate::sanitize_decide::at;
use crate::sanitize_decide::bundle;
use crate::sanitize_decide::endpoint;
use crate::sanitize_decide::mark;
use crate::sanitize_decide::model;
use crate::sanitize_decide::review;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Home {
    root: tempfile::TempDir,
}

impl Home {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for dir in ["state", "data", "tmp", "config"] {
            std::fs::create_dir_all(root.path().join(dir))?;
        }
        Ok(Self { root })
    }

    fn key(&self) -> PathBuf {
        self.root.path().join("state").join("sanitize.key")
    }

    fn mapping(&self) -> MappingFile {
        MappingFile::Default(self.root.path().join("config").join("mapping.toml"))
    }

    fn files(&self) -> Result<BTreeMap<PathBuf, (usize, u64)>, std::io::Error> {
        let mut files = BTreeMap::new();
        walk(self.root.path(), &mut files)?;
        Ok(files)
    }
}

fn walk(dir: &Path, files: &mut BTreeMap<PathBuf, (usize, u64)>) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.insert(path.clone(), (0, 0));
            walk(&path, files)?;
        } else {
            let bytes = std::fs::read(&path)?;
            let mut hasher = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(&bytes, &mut hasher);
            files.insert(path, (bytes.len(), std::hash::Hasher::finish(&hasher)));
        }
    }
    Ok(())
}

async fn drafted(
    home: &Home,
    url: String,
    mode: Mode,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let decider = Decider {
        endpoint: endpoint(url),
        review: review(20, mode)?,
    };
    let bundle = bundle()?;
    Ok(share::sanitize_in_memory(
        share::Job {
            bundle: &bundle,
            sessions: &BTreeMap::new(),
            mapping: &home.mapping(),
            key: &home.key(),
            decider: Some(&decider),
            at: at()?,
        },
        "s.clean.json",
    )
    .await?)
}

#[tokio::test]
async fn draft_writes_no_file() -> TestResult {
    let (stub, _) = model().await?;
    let home = Home::new()?;
    LocalKey::load_or_create(&home.key())?;
    let before = home.files()?;

    let outcome = drafted(&home, format!("http://{}", stub.addr), Mode::Block).await?;

    assert_eq!(home.files()?, before, "the draft wrote under the home");
    assert!(outcome.local.is_some(), "the draft holds its local review");
    assert!(outcome.markdown.contains("s.clean.json"));
    stub.close().await?;
    Ok(())
}

fn choice(number: usize, choice: FlagChoice) -> Mark {
    Mark {
        number,
        choice,
        learn: false,
    }
}

#[tokio::test]
async fn marks_json_matches_markdown_resolve() -> TestResult {
    let (stub, _) = model().await?;
    let home = Home::new()?;
    let outcome = drafted(&home, format!("http://{}", stub.addr), Mode::Block).await?;
    let local = outcome.local.ok_or("no local review")?;
    let key = LocalKey::load_or_create(&home.key())?;
    let bundle = outcome.sanitized.bundle;

    let report = mark(&outcome.markdown, 1, &format!("map {RISKY} as <customer>"));
    let report = mark(&report, 2, "redact");
    let by_markdown = sanitize::marks::read_flags(&report)?;
    let first_markdown = sanitize::resolve(&bundle, &by_markdown, &local, &key)?;
    let by_json = ReportFlags::from_choices(
        &local,
        &BTreeMap::new(),
        &[
            choice(
                1,
                FlagChoice::Map {
                    term: RISKY.to_owned(),
                    placeholder: "<customer>".to_owned(),
                },
            ),
            choice(2, FlagChoice::Redact),
        ],
    )?;
    let first_json = sanitize::resolve(&bundle, &by_json, &local, &key)?;
    assert_eq!(first_json.open, 1);
    assert_eq!(first_json.sealed.text, first_markdown.sealed.text);

    let applied: Vec<usize> = first_markdown
        .applied
        .iter()
        .map(|(number, _)| *number)
        .collect();
    let rewritten =
        by_markdown.rewrite(&report, first_markdown.open, first_markdown.total, &applied);
    let second_markdown = sanitize::resolve(
        &first_markdown.sealed.bundle,
        &sanitize::marks::read_flags(&mark(&rewritten, 3, "keep"))?,
        &local,
        &key,
    )?;
    let resolved: BTreeMap<_, _> = first_json.applied.into_iter().collect();
    let second_json = sanitize::resolve(
        &first_json.sealed.bundle,
        &ReportFlags::from_choices(&local, &resolved, &[choice(3, FlagChoice::Keep)])?,
        &local,
        &key,
    )?;
    assert_eq!(second_json.open, 0);
    assert_eq!(second_json.sealed.text, second_markdown.sealed.text);
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_json_map_runs_the_markdown_checks() -> TestResult {
    let (stub, _) = model().await?;
    let home = Home::new()?;
    let outcome = drafted(&home, format!("http://{}", stub.addr), Mode::Advisory).await?;
    let local = outcome.local.ok_or("no local review")?;
    let map = |term: &str, placeholder: &str| {
        ReportFlags::from_choices(
            &local,
            &BTreeMap::new(),
            &[choice(
                1,
                FlagChoice::Map {
                    term: term.to_owned(),
                    placeholder: placeholder.to_owned(),
                },
            )],
        )
        .err()
        .map(|error| error.to_string())
    };

    assert_eq!(
        map("Globex", "<customer>").as_deref(),
        Some("flag 1: the flagged text does not contain `Globex` as a whole word")
    );
    assert_eq!(
        map(RISKY, "[REDACTED:x]").as_deref(),
        Some(
            "flag 1: the placeholder `[REDACTED:x]` holds a `[FLAGGED:<n>]` or `[REDACTED...]` form; pick a plain placeholder"
        )
    );
    assert_eq!(
        map(RISKY, " ").as_deref(),
        Some("flag 1: the map placeholder is blank")
    );
    assert_eq!(
        ReportFlags::from_choices(&local, &BTreeMap::new(), &[choice(9, FlagChoice::Keep)])
            .err()
            .map(|error| error.to_string())
            .as_deref(),
        Some("flag 9 is not in this review")
    );
    assert_eq!(
        ReportFlags::from_choices(
            &local,
            &BTreeMap::new(),
            &[choice(1, FlagChoice::Keep), choice(1, FlagChoice::Redact)]
        )
        .err()
        .map(|error| error.to_string())
        .as_deref(),
        Some("flag 1 appears twice in the report")
    );
    stub.close().await?;
    Ok(())
}
