use std::path::Path;
use std::process::Command;

use rusqlite::Transaction;

use super::ImportError;

pub(super) fn clear_absent(tx: &Transaction<'_>, root: Option<&Path>) -> Result<(), ImportError> {
    clear_table(
        tx,
        root,
        "SELECT DISTINCT git_sha FROM ros_wiki_topics WHERE git_sha IS NOT NULL",
        "UPDATE ros_wiki_topics SET git_sha = NULL WHERE git_sha = ?1",
    )?;
    clear_table(
        tx,
        root,
        "SELECT DISTINCT git_sha FROM ros_wiki_extractions WHERE git_sha IS NOT NULL",
        "UPDATE ros_wiki_extractions SET git_sha = NULL WHERE git_sha = ?1",
    )?;
    clear_table(
        tx,
        root,
        "SELECT DISTINCT git_sha FROM ros_wiki_provenance WHERE git_sha IS NOT NULL",
        "UPDATE ros_wiki_provenance SET git_sha = NULL WHERE git_sha = ?1",
    )?;
    Ok(())
}

fn clear_table(
    tx: &Transaction<'_>,
    root: Option<&Path>,
    select: &str,
    update: &str,
) -> Result<(), ImportError> {
    let mut statement = tx.prepare(select)?;
    let shas = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for sha in shas {
        if let Some(root) = root
            && object_exists(root, &sha)?
        {
            continue;
        }
        tx.execute(update, [sha])?;
    }
    Ok(())
}

fn object_exists(root: &Path, sha: &str) -> Result<bool, ImportError> {
    let git_dir = root.join(".git");
    if !git_dir.is_dir() {
        return Ok(false);
    }
    let Ok(git_dir) = git_dir.into_os_string().into_string() else {
        return Err(ImportError::Read(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "wiki directory is not utf-8",
        )));
    };
    let output = Command::new("git")
        .args(["--git-dir", &git_dir, "cat-file", "-t", sha])
        .output()?;
    Ok(output.status.success()
        && std::str::from_utf8(&output.stdout).is_ok_and(|kind| kind.trim() == "commit"))
}
