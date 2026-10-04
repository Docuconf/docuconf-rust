//! JSON Schemas generated from the app's own types with `schemars`, and
//! checked with the `jsonschema` crate.

use serde::de::DeserializeOwned;

/// The JSON Schema `schemars` generates for `T`.
pub(crate) fn schema_for<T: schemars::JsonSchema>() -> serde_json::Value {
    let schema = schemars::SchemaGenerator::default().into_root_schema_for::<T>();
    serde_json::to_value(schema).unwrap_or_default()
}

/// Deserializes a JSON document into `T`, discarding the result: the check
/// that the document binds to the type the schema came from.
pub(crate) fn bind<T: DeserializeOwned>(v: &serde_json::Value) -> Result<(), String> {
    T::deserialize(v).map(|_| ()).map_err(|e| e.to_string())
}

/// Validates `doc` against `schema`, returning one message per error. For
/// secret inputs the messages name the location only, never the content.
pub(crate) fn validate(
    schema: &serde_json::Value,
    doc: &serde_json::Value,
    secret: bool,
) -> Vec<String> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => return vec![format!("has a schema that does not compile: {e}")],
    };
    validator
        .iter_errors(doc)
        .map(|e| {
            let at = e.instance_path().to_string();
            let at = if at.is_empty() { "/".to_string() } else { at };
            if secret {
                format!("at {at}: {}", e.masked())
            } else {
                format!("at {at}: {e}")
            }
        })
        .collect()
}
