use std::path::Path;
use std::path::PathBuf;

use config::Environment;

use crate::Error;

pub(crate) fn service_sqlite_path(path: PathBuf, env: &Environment) -> Result<PathBuf, Error> {
    if path.is_absolute() || path == Path::new(":memory:") {
        return Ok(path);
    }
    Ok(config::Paths::new(env)?.home().join(path))
}
