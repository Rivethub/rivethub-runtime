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

    pub fn scrub(&mut self, scrub: &mut impl Scrub) {
        scrub_members(&mut self.0, "", &[], scrub);
    }
}

pub trait Scrub {
    fn is_secret_name(&self, key: &str) -> bool;
    fn rank(&self, key: &str) -> u64;
    fn key(&mut self, parent: &str, key: &mut String) -> usize;
    fn renamed(&mut self, parent: &str, markers: (usize, Option<usize>), key: &str, renamed: &str);
    fn text(&mut self, path: &str, text: &mut String);
    fn secret(&mut self, path: &str, key: &str, text: &mut String) -> bool;
}

struct Member {
    original: String,
    key: String,
    marker: usize,
    value: Value,
}

fn scrub_members(
    members: &mut Map<String, Value>,
    parent: &str,
    under: &[String],
    scrub: &mut impl Scrub,
) {
    let mut entries: Vec<Member> = std::mem::take(members)
        .into_iter()
        .map(|(original, value)| {
            let mut key = original.clone();
            let marker = scrub.key(parent, &mut key);
            Member {
                original,
                key,
                marker,
                value,
            }
        })
        .collect();
    suffix_collisions(&mut entries, parent, scrub);
    for entry in entries {
        let mut named: Vec<String> = under.to_vec();
        for name in [&entry.original, &entry.key] {
            if scrub.is_secret_name(name) && !named.contains(name) {
                named.push(name.clone());
            }
        }
        let path = member_path(parent, &entry.key);
        let mut value = entry.value;
        scrub_value(&mut value, &path, &named, scrub);
        members.insert(entry.key, value);
    }
}

fn suffix_collisions(entries: &mut [Member], parent: &str, scrub: &mut impl Scrub) {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match groups.iter_mut().find(|(key, _)| *key == entry.key) {
            Some((_, members)) => members.push(index),
            None => groups.push((entry.key.clone(), vec![index])),
        }
    }
    let mut taken: Vec<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    let ends: Vec<Option<usize>> = (0..entries.len())
        .map(|index| entries.get(index + 1).map(|next| next.marker))
        .collect();
    for (base, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|&index| {
            entries
                .get(index)
                .map(|entry| (scrub.rank(&entry.original), entry.original.clone()))
        });
        for &index in members.iter().skip(1) {
            let mut suffix = 2;
            let mut renamed = format!("{base}#{suffix}");
            while taken.contains(&renamed) {
                suffix += 1;
                renamed = format!("{base}#{suffix}");
            }
            taken.push(renamed.clone());
            let end = ends.get(index).copied().flatten();
            if let Some(entry) = entries.get_mut(index) {
                scrub.renamed(parent, (entry.marker, end), &base, &renamed);
                entry.key = renamed;
            }
        }
    }
}

fn scrub_value(value: &mut Value, path: &str, under: &[String], scrub: &mut impl Scrub) {
    match value {
        Value::String(text) => {
            let replaced = under.iter().any(|key| scrub.secret(path, key, text));
            if !replaced {
                scrub.text(path, text);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                scrub_value(item, &format!("{path}[{index}]"), under, scrub);
            }
        }
        Value::Object(members) => scrub_members(members, path, under, scrub),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[must_use]
pub fn member_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_json;

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

    #[derive(Serialize)]
    enum Shape {
        Unit,
        Newtype(JsonObject),
        Tuple(u8, String),
        Struct { zeta: Option<u8>, alpha: f64 },
    }

    #[derive(Serialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum Tagged {
        Raw,
        Stamped { policy: String, count: u32 },
    }

    #[derive(Serialize)]
    struct Covered {
        version: u32,
        name: String,
        object: JsonObject,
        shapes: Vec<Shape>,
        tagged: Vec<Tagged>,
        missing: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped: Option<String>,
        bytes: Vec<u8>,
    }

    #[test]
    fn canonical_json_matches_sorting_a_parsed_value() {
        let object: JsonObject = serde_json::from_str(
            r#"{"z":[{"b":1,"a":[3,{"d":null,"c":true}]}],"é":"ü \" \\ \n \u0001","a":-1.5e300,"m":18446744073709551615,"":0}"#,
        )
        .unwrap();
        let sample = Covered {
            version: 2,
            name: "línea \u{1F600} \"quoted\"".to_owned(),
            object: object.clone(),
            shapes: vec![
                Shape::Unit,
                Shape::Newtype(object),
                Shape::Tuple(7, "x".to_owned()),
                Shape::Struct {
                    zeta: None,
                    alpha: 0.1,
                },
            ],
            tagged: vec![
                Tagged::Raw,
                Tagged::Stamped {
                    policy: "p".to_owned(),
                    count: 3,
                },
            ],
            missing: None,
            skipped: None,
            bytes: vec![0, 255],
        };
        let oracle = serde_json::to_vec(&sorted(serde_json::to_value(&sample).unwrap())).unwrap();
        assert_eq!(
            String::from_utf8(canonical_json(&sample).unwrap()).unwrap(),
            String::from_utf8(oracle).unwrap()
        );
    }

    #[test]
    fn canonical_json_writes_non_ascii_as_utf8_and_escapes_only_quotes_backslashes_and_controls() {
        assert_eq!(
            String::from_utf8(canonical_json("é \u{1F600} \" \\ \u{1} \u{7f}").unwrap()).unwrap(),
            "\"é \u{1F600} \\\" \\\\ \\u0001 \u{7f}\""
        );
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

    struct Recorder {
        secret_keys: Vec<&'static str>,
        seen: Vec<String>,
    }

    impl Scrub for Recorder {
        fn is_secret_name(&self, key: &str) -> bool {
            self.secret_keys.contains(&key)
        }

        fn rank(&self, key: &str) -> u64 {
            u64::from(key != "Matt")
        }

        fn key(&mut self, _parent: &str, key: &mut String) -> usize {
            if key == "Matt" {
                "matt".clone_into(key);
            }
            self.seen.len()
        }

        fn renamed(
            &mut self,
            parent: &str,
            markers: (usize, Option<usize>),
            key: &str,
            renamed: &str,
        ) {
            self.seen.push(format!(
                "renamed {parent}/{key}->{renamed} from {}",
                markers.0
            ));
        }

        fn text(&mut self, path: &str, text: &mut String) {
            self.seen.push(format!("text {path}={text}"));
            text.push('!');
        }

        fn secret(&mut self, path: &str, key: &str, text: &mut String) -> bool {
            self.seen.push(format!("secret {path} under {key}={text}"));
            if text == "keep" {
                return false;
            }
            "[X]".clone_into(text);
            true
        }
    }

    #[test]
    fn scrubbing_renames_collisions_before_descending_and_inherits_the_secret_key_downward() {
        let mut object: JsonObject = serde_json::from_str(
            r#"{"Matt": {"inner": "a"}, "matt": "b", "n": 1, "token": {"v": "x", "k": "keep", "count": 2, "list": ["y"]}}"#,
        )
        .unwrap();
        let mut recorder = Recorder {
            secret_keys: vec!["token"],
            seen: Vec::new(),
        };
        object.scrub(&mut recorder);
        assert_eq!(
            recorder.seen,
            [
                "renamed /matt->matt#2 from 0",
                "text matt.inner=a",
                "text matt#2=b",
                "secret token.v under token=x",
                "secret token.k under token=keep",
                "text token.k=keep",
                "secret token.list[0] under token=y",
            ]
        );
        assert_eq!(
            serde_json::to_string(&object).unwrap(),
            r#"{"matt":{"inner":"a!"},"matt#2":"b!","n":1,"token":{"v":"[X]","k":"keep!","count":2,"list":["[X]"]}}"#
        );
    }
}
