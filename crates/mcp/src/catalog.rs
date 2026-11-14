use crate::{memory, sessions, tags, wiki};
use rmcp::model::{Tool, ToolAnnotations};
use schemars::{JsonSchema, Schema, generate::SchemaSettings};

pub(crate) trait ToolInput: JsonSchema {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    fn annotations() -> Option<ToolAnnotations>;
}

fn definition<T: ToolInput>() -> Result<Tool, serde_json::Error> {
    let schema = SchemaSettings::draft2020_12()
        .with(|settings| settings.inline_subschemas = true)
        .with_transform(recorded_schema_metadata)
        .into_generator()
        .into_root_schema_for::<T>();
    let mut tool = Tool::new(
        T::NAME,
        T::DESCRIPTION,
        serde_json::from_value::<rmcp::model::JsonObject>(serde_json::to_value(schema)?)?,
    );
    tool.annotations = T::annotations();
    Ok(tool)
}

pub fn definitions() -> Result<Vec<Tool>, serde_json::Error> {
    Ok(vec![
        definition::<memory::Browse>()?,
        definition::<memory::GetFull>()?,
        definition::<memory::Search>()?,
        definition::<memory::Stats>()?,
        definition::<crate::team::Args>()?,
        definition::<tags::Args>()?,
        definition::<wiki::Read>()?,
        definition::<wiki::Search>()?,
    ])
}

pub fn session_definitions() -> Result<Vec<Tool>, serde_json::Error> {
    Ok(vec![
        definition::<sessions::Find>()?,
        definition::<sessions::Sanitize>()?,
        definition::<sessions::Import>()?,
    ])
}

pub fn recorded_schema_metadata(schema: &mut Schema) {
    schema.remove("title");
    schema.remove("format");
    schema.remove("default");
    schemars::transform::transform_subschemas(&mut recorded_schema_metadata, schema);
}

pub fn object_properties(schema: &mut Schema) {
    if schema.get("properties").is_none() {
        let empty: rmcp::model::JsonObject = rmcp::model::JsonObject::default();
        schema.insert("properties".into(), empty.into());
    }
}

pub fn optional_non_null(schema: &mut Schema) {
    if let Some(types) = schema
        .get_mut("type")
        .and_then(|value| value.as_array_mut())
    {
        types.retain(|value| value != "null");
        if types.len() == 1 {
            let value = types.remove(0);
            schema.insert("type".into(), value);
        }
    }
    if let Some(variants) = schema
        .get_mut("enum")
        .and_then(|value| value.as_array_mut())
    {
        variants.retain(|value| !value.is_null());
    }
}

pub fn search_mode_order(schema: &mut Schema) {
    if let Some(variants) = schema
        .get_mut("enum")
        .and_then(|value| value.as_array_mut())
        && let Some(index) = variants.iter().position(|value| value == "vector")
    {
        let vector = variants.remove(index);
        variants.push(vector);
    }
}
