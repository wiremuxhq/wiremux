//! Bidirectional request encode/decode for the four v1 dialects.

mod chat;
mod gemini;
mod messages;
mod responses;
mod tools;

use serde_json::Value;
use wiremux_auth::{ForbiddenFieldPolicy, ResolvedProfile, ToolNameCase, Wire};

use crate::ir::{IrItem, IrRequest, LossAction, LossReport};

/// Failure from request decode or encode.
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    /// Body is not valid JSON.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Policy refused the map (never a silent strip).
    #[error("hard-error at {path}: {detail}")]
    HardError {
        /// JSON-ish path of the refused field.
        path: String,
        /// Why the map failed.
        detail: String,
    },
    /// Request shape cannot be interpreted.
    #[error("{0}")]
    Invalid(String),
}

impl MapError {
    fn hard(path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::HardError {
            path: path.into(),
            detail: detail.into(),
        }
    }
}

/// Decode a dialect request body into IR. Does not apply profile policy.
pub fn decode(wire: Wire, bytes: &[u8]) -> Result<(IrRequest, LossReport), MapError> {
    let value: Value = serde_json::from_slice(bytes)?;
    if !value.is_object() {
        return Err(MapError::Invalid(
            "request body must be a JSON object".into(),
        ));
    }
    match wire {
        Wire::ChatCompletions => chat::decode(&value),
        Wire::Messages => messages::decode(&value),
        Wire::Responses => responses::decode(&value),
        Wire::Gemini => gemini::decode(&value),
    }
}

/// Encode IR into a dialect request body. Applies `tool_type_policy` and fingerprint.
pub fn encode(
    wire: Wire,
    ir: &IrRequest,
    profile: &ResolvedProfile,
) -> Result<(Vec<u8>, LossReport), MapError> {
    let mut report = LossReport::default();
    let mut ir = apply_system_prefix(ir, profile);
    strip_or_refuse_store(&mut ir, profile, &mut report)?;
    let prepared = tools::prepare_tools(wire, &ir, profile, &mut report)?;
    let mut body = match wire {
        Wire::ChatCompletions => chat::encode(&ir, &prepared, &mut report)?,
        Wire::Messages => messages::encode(&ir, &prepared, &mut report)?,
        Wire::Responses => responses::encode(&ir, &prepared, profile, &mut report)?,
        Wire::Gemini => gemini::encode(&ir, &prepared, &mut report)?,
    };
    merge_extra_body(&mut body, profile);
    apply_forbidden_fields(&mut body, profile, &mut report)?;
    Ok((serde_json::to_vec(&body)?, report))
}

fn apply_system_prefix(ir: &IrRequest, profile: &ResolvedProfile) -> IrRequest {
    let mut out = ir.clone();
    let Some(prefix) = profile
        .fingerprint
        .as_ref()
        .and_then(|fp| fp.system_prompt_prefix.as_deref())
    else {
        return out;
    };
    if prefix.is_empty() {
        return out;
    }
    match out
        .items
        .iter_mut()
        .find(|item| matches!(item, IrItem::System { .. }))
    {
        Some(IrItem::System { text }) => {
            if !text.starts_with(prefix) {
                *text = format!("{prefix}{text}");
            }
        }
        _ => out.items.insert(
            0,
            IrItem::System {
                text: prefix.to_string(),
            },
        ),
    }
    out
}

fn strip_or_refuse_store(
    ir: &mut IrRequest,
    profile: &ResolvedProfile,
    report: &mut LossReport,
) -> Result<(), MapError> {
    let Some(fp) = profile.fingerprint.as_ref() else {
        return Ok(());
    };
    if ir.sampling.store.is_none() {
        return Ok(());
    }
    if !fp.forbidden_body_fields.iter().any(|key| key == "store") {
        return Ok(());
    }
    match fp.forbidden_field_policy {
        ForbiddenFieldPolicy::HardError => Err(MapError::hard(
            "sampling.store",
            "forbidden body field `store`",
        )),
        ForbiddenFieldPolicy::Strip => {
            ir.sampling.store = None;
            report.record(
                "sampling.store",
                LossAction::Drop,
                "forbidden_body_fields strip",
            );
            Ok(())
        }
    }
}

fn merge_extra_body(body: &mut Value, profile: &ResolvedProfile) {
    let Some(fp) = profile.fingerprint.as_ref() else {
        return;
    };
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    for (key, value) in &fp.extra_body {
        obj.insert(key.clone(), value.clone());
    }
}

fn apply_forbidden_fields(
    body: &mut Value,
    profile: &ResolvedProfile,
    report: &mut LossReport,
) -> Result<(), MapError> {
    let Some(fp) = profile.fingerprint.as_ref() else {
        return Ok(());
    };
    let Some(obj) = body.as_object_mut() else {
        return Ok(());
    };
    for key in &fp.forbidden_body_fields {
        if !obj.contains_key(key) {
            continue;
        }
        match fp.forbidden_field_policy {
            ForbiddenFieldPolicy::HardError => {
                return Err(MapError::hard(
                    key.clone(),
                    format!("forbidden body field `{key}`"),
                ));
            }
            ForbiddenFieldPolicy::Strip => {
                obj.remove(key);
                report.record(key.clone(), LossAction::Drop, "forbidden_body_fields strip");
            }
        }
    }
    Ok(())
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn split_data_url(url: &str) -> Option<(&str, &str)> {
    url.strip_prefix("data:")?.split_once(";base64,")
}

fn f32_field(value: &Value, key: &str) -> Option<f32> {
    value.get(key)?.as_f64().map(|n| n as f32)
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn is_gemini_file_raw(raw: &Value) -> bool {
    raw.get("fileData").is_some() || raw.get("fileUri").is_some()
}

fn raw_type_name(raw: &Value) -> Option<&str> {
    raw.get("type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Messages-shaped Raw has a nonempty `type` and no Gemini file keys.
fn messages_raw_passthrough(raw: &Value) -> bool {
    raw_type_name(raw).is_some() && !is_gemini_file_raw(raw)
}

/// Responses-shaped Raw has a nonempty `type` and no Gemini file keys.
fn responses_raw_passthrough(raw: &Value) -> bool {
    messages_raw_passthrough(raw)
}

fn off_dialect_raw_path(raw: &Value) -> &'static str {
    if raw.get("fileData").is_some() {
        "item.part.fileData"
    } else if raw.get("fileUri").is_some() {
        "item.part.fileUri"
    } else {
        "item.part.raw"
    }
}

fn stop_values(value: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(raw) = value.get(*key) else {
            continue;
        };
        if let Some(s) = raw.as_str() {
            return vec![s.to_string()];
        }
        if let Some(arr) = raw.as_array() {
            return arr
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect();
        }
    }
    Vec::new()
}

/// Official OpenAI json_schema needs a nonempty name and an object schema.
fn official_json_schema<'a>(
    schema: &'a Value,
    name: Option<&'a str>,
    report: &mut LossReport,
) -> Option<(&'a Value, &'a str)> {
    let name = name.map(str::trim).filter(|s| !s.is_empty());
    match name {
        Some(name) if schema.is_object() => Some((schema, name)),
        _ => {
            report.record(
                "sampling.json_schema",
                LossAction::Drop,
                "json_schema requires nonempty name and object schema",
            );
            None
        }
    }
}

fn value_as_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn apply_tool_name_case(name: &str, case: Option<ToolNameCase>) -> String {
    match case {
        None | Some(ToolNameCase::AsIs) => name.to_string(),
        Some(ToolNameCase::Snake) => convert_case(name, '_'),
        Some(ToolNameCase::Kebab) => convert_case(name, '-'),
    }
}

fn convert_case(name: &str, sep: char) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch == '-' || ch == '_' {
            out.push(sep);
            continue;
        }
        if ch.is_uppercase() {
            if i > 0 && !out.ends_with(sep) && !out.ends_with('.') {
                out.push(sep);
            }
            out.extend(ch.to_lowercase());
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_and_kebab_leave_dots() {
        assert_eq!(convert_case("crm.lookupId", '_'), "crm.lookup_id");
        assert_eq!(convert_case("crm.lookup_id", '-'), "crm.lookup-id");
    }
}
