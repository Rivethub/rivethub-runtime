use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Extra(Map<String, Value>);

impl Extra {
    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct JsonObject(Map<String, Value>);

impl JsonObject {
    pub fn from_serialize<T: Serialize + ?Sized>(value: &T) -> Result<Self, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(value)?)
    }

    pub fn deserialize<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(Value::Object(self.0.clone()))
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    #[must_use]
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn insert<T: Serialize>(&mut self, key: &str, value: T) -> Result<(), serde_json::Error> {
        self.0.insert(key.to_owned(), serde_json::to_value(value)?);
        Ok(())
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.0.retain(|key, _| keep(key));
    }

    pub fn visit_strings_mut(&mut self, visit: &mut impl FnMut(&str, &mut String)) {
        for (key, value) in &mut self.0 {
            visit_strings(key, value, visit);
        }
    }
}

pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&sorted(serde_json::to_value(value)?))
}

fn sorted(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        Value::Object(members) => {
            let mut entries: Vec<_> = members
                .into_iter()
                .map(|(name, member)| (name, sorted(member)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect())
        }
        leaf @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => leaf,
    }
}

fn visit_strings(key: &str, value: &mut Value, visit: &mut impl FnMut(&str, &mut String)) {
    match value {
        Value::String(text) => visit(key, text),
        Value::Array(items) => {
            for item in items {
                visit_strings(key, item, visit);
            }
        }
        Value::Object(members) => {
            for (name, member) in members {
                visit_strings(name, member, visit);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, schemars::JsonSchema)]
    struct Sample {
        name: &'static str,
        count: u32,
    }

    #[test]
    fn objects_are_built_and_edited_through_serde() {
        let mut object = JsonObject::from_serialize(&Sample {
            name: "a",
            count: 2,
        })
        .unwrap();
        object.insert("flag", true).unwrap();
        object.retain(|key| key != "count");
        assert_eq!(
            serde_json::to_string(&object).unwrap(),
            r#"{"name":"a","flag":true}"#
        );
        assert!(JsonObject::from_serialize(&[1, 2]).is_err());
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth_and_keeps_array_order() {
        let object: JsonObject =
            serde_json::from_str(r#"{ "b": [ {"z": 1, "y": 2}, 3 ], "a": null }"#).unwrap();
        assert_eq!(
            String::from_utf8(canonical_json(&object).unwrap()).unwrap(),
            r#"{"a":null,"b":[{"y":2,"z":1},3]}"#
        );
    }

    #[test]
    fn string_leaves_are_visited_with_their_member_name() {
        let mut object: JsonObject =
            serde_json::from_str(r#"{"a":"x","list":["y",{"b":"z"}],"n":1,"none":null}"#).unwrap();
        let mut seen = Vec::new();
        object.visit_strings_mut(&mut |key, text| {
            seen.push(format!("{key}={text}"));
            text.push('!');
        });
        assert_eq!(seen, ["a=x", "list=y", "b=z"]);
        assert_eq!(
            serde_json::to_string(&object).unwrap(),
            r#"{"a":"x!","list":["y!",{"b":"z!"}],"n":1,"none":null}"#
        );
    }
}
