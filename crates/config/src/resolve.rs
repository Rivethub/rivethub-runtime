use regress::Regex;

use crate::ConfigError;
use crate::environment::Environment;

pub fn resolve_env_vars_with(
    env: &Environment,
    value: &mut serde_yaml_ng::Value,
) -> Result<(), ConfigError> {
    let pattern = Regex::new(r"\$\{[A-Za-z0-9_]+\}")?;
    resolve(env, value, &pattern)
}

fn resolve(
    env: &Environment,
    value: &mut serde_yaml_ng::Value,
    pattern: &Regex,
) -> Result<(), ConfigError> {
    match value {
        serde_yaml_ng::Value::String(text) => *text = substitute(env, text, pattern)?,
        serde_yaml_ng::Value::Sequence(items) => {
            for item in items {
                resolve(env, item, pattern)?;
            }
        }
        serde_yaml_ng::Value::Mapping(entries) => {
            for (_key, item) in entries.iter_mut() {
                resolve(env, item, pattern)?;
            }
        }
        serde_yaml_ng::Value::Tagged(tagged) => resolve(env, &mut tagged.value, pattern)?,
        serde_yaml_ng::Value::Null
        | serde_yaml_ng::Value::Bool(_)
        | serde_yaml_ng::Value::Number(_) => {}
    }
    Ok(())
}

fn substitute(env: &Environment, text: &str, pattern: &Regex) -> Result<String, ConfigError> {
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    for found in pattern.find_iter(text) {
        out.push_str(&text[start..found.start()]);
        let name = &text[found.start() + 2..found.end() - 1];
        let value = env
            .get(name)
            .map(|value| {
                value.to_str().ok_or_else(|| ConfigError::EnvironmentText {
                    name: name.to_owned(),
                })
            })
            .transpose()?;
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let replacement = value.unwrap_or("");
        out.push_str(replacement);
        start = found.end();
    }
    out.push_str(&text[start..]);
    Ok(out)
}
