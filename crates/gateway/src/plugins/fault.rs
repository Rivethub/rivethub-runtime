use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;

pub(crate) fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(&mut decoder).map_err(|error| {
        let path = error.path().to_string();
        format!("{path}: {}", error.into_inner())
    })
}

pub(crate) fn parse_answer<T: DeserializeOwned + JsonSchema>(bytes: &[u8]) -> Result<T, String> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(&mut decoder).map_err(|error| {
        let coarse = error.path().to_string();
        let said = format!("{coarse}: {}", error.into_inner());
        located::<T>(bytes, &coarse).unwrap_or(said)
    })
}

fn located<T: JsonSchema>(bytes: &[u8], coarse: &str) -> Option<String> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return None;
    };
    let schema = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<T>();
    let root = schema.as_value();
    let walk = Walk {
        defs: root.get("$defs").and_then(Value::as_object),
    };
    let fault = walk.check(root, &value, &mut Vec::new())?;
    let path = fault.path();
    (coarse == "." || path.starts_with(coarse)).then(|| format!("{path}: {}", fault.why))
}

#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Index(usize),
}

#[derive(Debug)]
struct Fault {
    at: Vec<Step>,
    why: String,
}

impl Fault {
    fn new(at: &[Step], why: String) -> Self {
        Self {
            at: at.to_vec(),
            why,
        }
    }

    fn path(&self) -> String {
        let mut path = String::new();
        for step in &self.at {
            match step {
                Step::Key(key) if path.is_empty() => path.push_str(key),
                Step::Key(key) => {
                    path.push('.');
                    path.push_str(key);
                }
                Step::Index(index) => {
                    path.push('[');
                    path.push_str(&index.to_string());
                    path.push(']');
                }
            }
        }
        if path.is_empty() {
            path.push('.');
        }
        path
    }
}

struct Walk<'a> {
    defs: Option<&'a Map<String, Value>>,
}

impl<'a> Walk<'a> {
    fn resolve(&self, schema: &'a Value) -> &'a Value {
        let target = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|target| target.strip_prefix("#/$defs/"));
        match (target, self.defs) {
            (Some(name), Some(defs)) => match defs.get(name) {
                Some(found) => self.resolve(found),
                None => schema,
            },
            _ => schema,
        }
    }

    fn check(&self, schema: &'a Value, value: &Value, at: &mut Vec<Step>) -> Option<Fault> {
        match self.resolve(schema) {
            Value::Bool(false) => Some(Fault::new(at, "is not allowed here".to_owned())),
            Value::Object(rules) => self.rules(rules, value, at),
            Value::Null
            | Value::Bool(true)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => None,
        }
    }

    fn rules(
        &self,
        rules: &'a Map<String, Value>,
        value: &Value,
        at: &mut Vec<Step>,
    ) -> Option<Fault> {
        for key in ["oneOf", "anyOf"] {
            if let Some(options) = rules.get(key).and_then(Value::as_array)
                && let Some(fault) = self.choose(options, value, at)
            {
                return Some(fault);
            }
        }
        if let Some(all) = rules.get("allOf").and_then(Value::as_array)
            && let Some(fault) = all.iter().find_map(|schema| self.check(schema, value, at))
        {
            return Some(fault);
        }
        if let Some(expected) = rules.get("const").filter(|expected| *expected != value) {
            return Some(Fault::new(
                at,
                format!("invalid value: {}, expected {expected}", shown(value)),
            ));
        }
        if let Some(choices) = rules
            .get("enum")
            .and_then(Value::as_array)
            .filter(|choices| !choices.contains(value))
        {
            return Some(Fault::new(
                at,
                format!(
                    "unknown variant {}, expected one of {}",
                    shown(value),
                    listed(choices)
                ),
            ));
        }
        if let Some(kinds) = rules.get("type").filter(|kinds| !fits(kinds, value)) {
            return Some(Fault::new(
                at,
                format!("invalid type: {}, expected {}", shown(value), named(kinds)),
            ));
        }
        match value {
            Value::Object(fields) => self.object(rules, fields, at),
            Value::Array(items) => self.array(rules, items, at),
            Value::Number(_) => bounded(rules, value, at),
            Value::Null | Value::Bool(_) | Value::String(_) => None,
        }
    }

    fn choose(&self, options: &'a [Value], value: &Value, at: &mut Vec<Step>) -> Option<Fault> {
        let tag = value.get("type").and_then(Value::as_str);
        let tagged: Vec<(&str, &'a Value)> = options
            .iter()
            .filter_map(|option| {
                let resolved = self.resolve(option);
                let name = resolved
                    .pointer("/properties/type/const")
                    .and_then(Value::as_str)?;
                Some((name, resolved))
            })
            .collect();
        if let (Some(tag), false) = (tag, tagged.is_empty()) {
            let Some((_, schema)) = tagged.iter().find(|(name, _)| *name == tag) else {
                at.push(Step::Key("type".to_owned()));
                let names: Vec<&str> = tagged.iter().map(|(name, _)| *name).collect();
                let fault = Fault::new(
                    at,
                    format!(
                        "unknown variant `{tag}`, expected one of {}",
                        names.join(", ")
                    ),
                );
                at.pop();
                return Some(fault);
            };
            return self.check(schema, value, at);
        }
        let mut faults = Vec::new();
        for option in options {
            faults.push(self.check(option, value, at)?);
        }
        faults.into_iter().max_by_key(|fault| fault.at.len())
    }

    fn object(
        &self,
        rules: &'a Map<String, Value>,
        fields: &Map<String, Value>,
        at: &mut Vec<Step>,
    ) -> Option<Fault> {
        let properties = rules.get("properties").and_then(Value::as_object);
        let additional = rules.get("additionalProperties");
        for (key, field) in fields {
            let schema = properties
                .and_then(|properties| properties.get(key))
                .or(additional);
            let Some(schema) = schema else {
                continue;
            };
            if *schema == Value::Bool(false) {
                at.push(Step::Key(key.clone()));
                let fault = Fault::new(at, format!("unknown field `{key}`"));
                at.pop();
                return Some(fault);
            }
            at.push(Step::Key(key.clone()));
            let fault = self.check(schema, field, at);
            at.pop();
            if fault.is_some() {
                return fault;
            }
        }
        rules
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|name| !fields.contains_key(*name))
            .map(|name| Fault::new(at, format!("missing field `{name}`")))
    }

    fn array(
        &self,
        rules: &'a Map<String, Value>,
        items: &[Value],
        at: &mut Vec<Step>,
    ) -> Option<Fault> {
        let schema = rules.get("items")?;
        for (index, item) in items.iter().enumerate() {
            at.push(Step::Index(index));
            let fault = self.check(schema, item, at);
            at.pop();
            if fault.is_some() {
                return fault;
            }
        }
        None
    }
}

fn bounded(rules: &Map<String, Value>, number: &Value, at: &[Step]) -> Option<Fault> {
    let value = number.as_f64()?;
    if let Some(minimum) = rules
        .get("minimum")
        .and_then(Value::as_f64)
        .filter(|minimum| value < *minimum)
    {
        return Some(Fault::new(
            at,
            format!("invalid value: {number}, expected at least {minimum}"),
        ));
    }
    rules
        .get("maximum")
        .and_then(Value::as_f64)
        .filter(|maximum| value > *maximum)
        .map(|maximum| {
            Fault::new(
                at,
                format!("invalid value: {number}, expected at most {maximum}"),
            )
        })
}

fn fits(kinds: &Value, value: &Value) -> bool {
    let fits_one = |kind: &str| match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => true,
    };
    match kinds {
        Value::String(kind) => fits_one(kind),
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).any(fits_one),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => true,
    }
}

fn named(kinds: &Value) -> String {
    let article = |kind: &str| match kind {
        "null" => "null".to_owned(),
        "integer" => "an integer".to_owned(),
        "object" => "an object".to_owned(),
        "array" => "an array".to_owned(),
        other => format!("a {other}"),
    };
    match kinds {
        Value::String(kind) => article(kind),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .map(article)
            .collect::<Vec<_>>()
            .join(" or "),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => kinds.to_string(),
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => format!("boolean `{flag}`"),
        Value::Number(number) if number.is_f64() => format!("floating point `{number}`"),
        Value::Number(number) => format!("integer `{number}`"),
        Value::String(text) => format!("string {text:?}"),
        Value::Array(_) => "a sequence".to_owned(),
        Value::Object(_) => "a map".to_owned(),
    }
}

fn listed(choices: &[Value]) -> String {
    choices
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
