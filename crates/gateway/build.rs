use std::io;
use std::path::Path;
use std::process::Command;

fn git(args: &[impl AsRef<std::ffi::OsStr>]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("git could not resolve the build revision").into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn watch(path: &str) {
    println!("cargo:rerun-if-changed={path}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let git_file = Path::new("../../.git");
    watch("../../.git");
    if !git_file.try_exists()? {
        return Ok(());
    }
    watch(&git(&["rev-parse", "--git-path", "logs/HEAD"])?);
    watch(&git(&["rev-parse", "--git-path", "HEAD"])?);
    if let Ok(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch(&git(&["rev-parse", "--git-path", branch.as_str()])?);
    }
    println!(
        "cargo:rustc-env=RIVETHUB_BUILD_REVISION={}",
        git(&["rev-parse", "--short", "HEAD"])?
    );
    Ok(())
}
