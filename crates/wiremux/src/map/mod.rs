//! Bidirectional request encode/decode for the v1 dialects.

mod chat;
mod converse;
mod gemini;
mod messages;
mod responses;
mod tools;

pub(crate) use converse::converse_service_tier;
pub(crate) use gemini::{gemini_decode_service_tier, gemini_service_tier};

use std::collections::BTreeMap;

use serde_json::Value;
use wiremux_auth::{ForbiddenFieldPolicy, ResolvedProfile, ToolNameCase, Wire};

use crate::ir::{IrDocumentSource, IrItem, IrPart, IrRequest, IrSampling, LossAction, LossReport};

/// Failure from request decode or encode.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A JSON object field. A non-object stays visible under `wrap_key` and
/// is a Degrade. An empty object would drop the caller's text.
pub(super) fn json_object_or_raw(
    raw_text: &str,
    path: &str,
    report: &mut LossReport,
    detail: &str,
    wrap_key: &str,
) -> Value {
    let mut wrap = |value: Value| {
        report.record(path, LossAction::Degrade, detail);
        let mut map = serde_json::Map::new();
        map.insert(wrap_key.to_string(), value);
        Value::Object(map)
    };
    match serde_json::from_str::<Value>(raw_text) {
        Ok(value) if value.is_object() => value,
        Ok(value) => wrap(value),
        Err(_) => wrap(Value::String(raw_text.to_string())),
    }
}

/// Decode a dialect request body into IR. Does not apply profile policy.
pub fn decode(wire: Wire, bytes: &[u8]) -> Result<(IrRequest, LossReport), MapError> {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(err) => {
            return Err(MapError::Invalid(format!(
                "request body for wire `{}` is invalid JSON: {err}",
                wire.as_str()
            )));
        }
    };
    if !value.is_object() {
        return Err(MapError::Invalid(format!(
            "request body for wire `{}` must be a JSON object, got {}",
            wire.as_str(),
            json_kind(&value)
        )));
    }
    match wire {
        Wire::ChatCompletions => chat::decode(&value),
        Wire::Messages => messages::decode(&value),
        Wire::Responses => responses::decode(&value),
        Wire::Gemini => gemini::decode(&value),
        Wire::Converse => converse::decode(&value),
        _ => Err(MapError::Invalid(format!(
            "unsupported wire `{}`",
            wire.as_str()
        ))),
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
        Wire::Converse => converse::encode(&ir, &prepared, &mut report)?,
        _ => {
            return Err(MapError::Invalid(format!(
                "unsupported wire `{}`",
                wire.as_str()
            )));
        }
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

fn drop_dest_chat_sampling_extras(s: &IrSampling, report: &mut LossReport) {
    if s.prompt_cache_mode.is_some() {
        report.record("sampling.prompt_cache_mode", LossAction::Drop, "no slot");
    }
    if s.prompt_cache_ttl.is_some() {
        report.record("sampling.prompt_cache_ttl", LossAction::Drop, "no slot");
    }
    if s.moderation_model.is_some() {
        report.record("sampling.moderation_model", LossAction::Drop, "no slot");
    }
    if s.moderation_input.is_some() {
        report.record("sampling.moderation_input", LossAction::Drop, "no slot");
    }
    if s.moderation_output.is_some() {
        report.record("sampling.moderation_output", LossAction::Drop, "no slot");
    }
    if s.include_obfuscation.is_some() {
        report.record("sampling.include_obfuscation", LossAction::Drop, "no slot");
    }
    drop_dest_chat_logit_bias(s, report);
}

fn drop_dest_chat_logit_bias(s: &IrSampling, report: &mut LossReport) {
    if !s.logit_bias.is_empty() {
        report.record("sampling.logit_bias", LossAction::Drop, "no slot");
    }
    if s.prediction.is_some() {
        report.record("sampling.prediction", LossAction::Drop, "no slot");
    }
    if s.web_search_options.is_some() {
        report.record("sampling.web_search_options", LossAction::Drop, "no slot");
    }
}

fn drop_dest_top_logprobs(s: &IrSampling, report: &mut LossReport) {
    if s.top_logprobs.is_some() {
        report.record("sampling.top_logprobs", LossAction::Drop, "no slot");
    }
}

fn drop_dest_logprobs(s: &IrSampling, report: &mut LossReport) {
    if s.logprobs.is_some() {
        report.record("sampling.logprobs", LossAction::Drop, "no slot");
    }
}

fn drop_dest_n_and_penalties(s: &IrSampling, report: &mut LossReport) {
    if s.frequency_penalty.is_some() {
        report.record("sampling.frequency_penalty", LossAction::Drop, "no slot");
    }
    if s.presence_penalty.is_some() {
        report.record("sampling.presence_penalty", LossAction::Drop, "no slot");
    }
    if s.seed.is_some() {
        report.record("sampling.seed", LossAction::Drop, "no slot");
    }
    if s.n.is_some() {
        report.record("sampling.n", LossAction::Drop, "no slot");
    }
    if s.media_resolution.is_some() {
        report.record("sampling.media_resolution", LossAction::Drop, "no slot");
    }
    if s.image_config.is_some() {
        report.record("sampling.image_config", LossAction::Drop, "no slot");
    }
    if s.audio_transcription_config.is_some() {
        report.record(
            "sampling.audio_transcription_config",
            LossAction::Drop,
            "no slot",
        );
    }
    if s.translation_config.is_some() {
        report.record("sampling.translation_config", LossAction::Drop, "no slot");
    }
    if s.affective_dialog.is_some() {
        report.record("sampling.affective_dialog", LossAction::Drop, "no slot");
    }
    if s.enhanced_civic_answers.is_some() {
        report.record(
            "sampling.enhanced_civic_answers",
            LossAction::Drop,
            "no slot",
        );
    }
    if s.multi_speaker_speech.is_some() {
        report.record("sampling.multi_speaker_speech", LossAction::Drop, "no slot");
    }
    if s.cached_content.is_some() {
        report.record("sampling.cached_content", LossAction::Drop, "no slot");
    }
    if s.safety_settings.is_some() {
        report.record("sampling.safety_settings", LossAction::Drop, "no slot");
    }
    if s.request_labels.is_some() {
        report.record("sampling.request_labels", LossAction::Drop, "no slot");
    }
}

fn drop_dest_output_modalities(s: &IrSampling, report: &mut LossReport) {
    if !s.output_modalities.is_empty() {
        report.record("sampling.output_modalities", LossAction::Drop, "no slot");
    }
    if s.audio_voice.is_some() {
        report.record("sampling.audio_voice", LossAction::Drop, "no slot");
    }
    if s.audio_format.is_some() {
        report.record("sampling.audio_format", LossAction::Drop, "no slot");
    }
}

fn string_object_field(value: &Value, key: &str) -> BTreeMap<String, String> {
    let Some(obj) = value.get(key).and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    obj.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

fn split_data_url(url: &str) -> Option<(&str, &str)> {
    url.strip_prefix("data:")?.split_once(";base64,")
}

fn is_pdf_media_type(media: &str) -> bool {
    let media = media.to_ascii_lowercase();
    media == "application/pdf" || media == "application/x-pdf" || media == "pdf"
}

fn is_audio_media_type(media: &str) -> bool {
    media.to_ascii_lowercase().starts_with("audio/")
}

fn audio_format_from_mime(mime: &str) -> String {
    mime.split_once('/')
        .map(|(_, rest)| rest.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| mime.to_string())
}

fn audio_mime_from_format(format: &str) -> String {
    if format.contains('/') {
        format.to_string()
    } else {
        format!("audio/{format}")
    }
}

fn converse_document_format(media_type: &str) -> Option<&'static str> {
    let media = media_type.to_ascii_lowercase();
    match media.as_str() {
        "application/pdf" | "application/x-pdf" | "pdf" => Some("pdf"),
        "text/csv" | "csv" => Some("csv"),
        "application/msword" | "doc" => Some("doc"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" | "docx" => {
            Some("docx")
        }
        "application/vnd.ms-excel" | "xls" => Some("xls"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" | "xlsx" => {
            Some("xlsx")
        }
        "text/html" | "html" => Some("html"),
        "text/plain" | "txt" => Some("txt"),
        "text/markdown" | "md" => Some("md"),
        _ => None,
    }
}

fn media_type_from_converse_format(format: &str) -> String {
    match format.to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf".into(),
        "csv" => "text/csv".into(),
        "doc" => "application/msword".into(),
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
        "xls" => "application/vnd.ms-excel".into(),
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        "html" => "text/html".into(),
        "txt" => "text/plain".into(),
        "md" => "text/markdown".into(),
        other => other.to_string(),
    }
}

fn media_type_from_converse_image_format(format: &str) -> String {
    let format = format.trim();
    if format.contains('/') {
        return format.to_string();
    }
    match format.to_ascii_lowercase().as_str() {
        "png" => "image/png".into(),
        "jpeg" | "jpg" => "image/jpeg".into(),
        "gif" => "image/gif".into(),
        "webp" => "image/webp".into(),
        "" => "image/png".into(),
        other => format!("image/{other}"),
    }
}

pub(crate) fn converse_image_format(media_type: &str) -> Option<&'static str> {
    let media = media_type.to_ascii_lowercase();
    let media = media.strip_prefix("image/").unwrap_or(media.as_str());
    match media {
        "png" => Some("png"),
        "jpeg" | "jpg" => Some("jpeg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        _ => None,
    }
}

/// Inline bytes Gemini `functionResponse.parts` can carry.
pub(super) struct FunctionResponseInline {
    pub mime: String,
    pub data: String,
    pub display_name: Option<String>,
}

fn function_response_mime_allowed(mime: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    mime.starts_with("image/") || is_pdf_media_type(&mime) || mime == "text/plain"
}

/// Image bytes inside a function result, including a Gemini part that
/// kept `displayName` as raw `inlineData`.
pub(super) fn function_response_image(part: &IrPart) -> Option<(String, String)> {
    let inline = function_response_inline(part)?;
    inline
        .mime
        .to_ascii_lowercase()
        .starts_with("image/")
        .then_some((inline.mime, inline.data))
}

pub(super) fn part_has_media_hint(part: &Value) -> bool {
    part.get("mediaResolution").is_some()
        || part.get("mediaProcessing").is_some()
        || part.get("videoMetadata").is_some()
        || part.get("audioTranscription").is_some()
        || part.get("speechMetadata").is_some()
        || part.get("partMetadata").is_some()
        || part.get("thoughtSignature").is_some()
}

/// Bytes-only view of a Gemini part kept whole so `mediaResolution`,
/// `mediaProcessing`, or `videoMetadata` can round-trip.
pub(super) fn plain_media_part(part: &IrPart) -> Option<IrPart> {
    let IrPart::Raw { raw, .. } = part else {
        return None;
    };
    if !part_has_media_hint(raw) {
        return None;
    }
    if let Some(inline) = raw.get("inlineData") {
        let media = str_field(inline, "mimeType").unwrap_or_default();
        let data = str_field(inline, "data").unwrap_or_default();
        if data.is_empty() {
            return None;
        }
        if is_audio_media_type(&media) {
            return Some(IrPart::Audio {
                data,
                format: audio_format_from_mime(&media),
            });
        }
        if is_pdf_media_type(&media) {
            return Some(IrPart::Document {
                source: IrDocumentSource::Base64(data),
                media_type: media,
                name: None,
            });
        }
        return Some(IrPart::ImageBase64 {
            media_type: media,
            data,
        });
    }
    if let Some(text) = raw.get("text").and_then(Value::as_str) {
        return Some(IrPart::Text(text.to_string()));
    }
    let file = raw.get("fileData")?;
    let media = str_field(file, "mimeType").unwrap_or_default();
    let uri = str_field(file, "fileUri").unwrap_or_default();
    if is_pdf_media_type(&media) && !uri.is_empty() {
        return Some(IrPart::Document {
            source: document_ref_source(uri),
            media_type: media,
            name: None,
        });
    }
    None
}

pub(super) fn function_response_inline(part: &IrPart) -> Option<FunctionResponseInline> {
    let (mime, data, name) = match part {
        IrPart::ImageBase64 { media_type, data } => (media_type.clone(), data.clone(), None),
        IrPart::ImageUrl(url) => {
            let (mime, data) = split_data_url(url)?;
            (mime.to_string(), data.to_string(), None)
        }
        IrPart::Document {
            source: IrDocumentSource::Base64(data),
            media_type,
            name,
        } => (media_type.clone(), data.clone(), name.clone()),
        IrPart::Raw { raw, .. } => {
            let inline = raw.get("inlineData")?;
            (
                str_field(inline, "mimeType")?,
                str_field(inline, "data")?,
                str_field(inline, "displayName"),
            )
        }
        _ => return None,
    };
    if data.is_empty() || !function_response_mime_allowed(&mime) {
        return None;
    }
    Some(FunctionResponseInline {
        mime,
        data,
        display_name: name.filter(|item| !item.is_empty()),
    })
}

fn converse_image_format_from_url(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "png",
        "jpg" | "jpeg" => "jpeg",
        "gif" => "gif",
        "webp" => "webp",
        _ => "png",
    }
}

fn converse_audio_format(format: &str) -> Option<&'static str> {
    let lower = format.to_ascii_lowercase();
    let lower = lower.strip_prefix("audio/").unwrap_or(&lower);
    Some(match lower {
        "mp3" => "mp3",
        "opus" => "opus",
        "wav" => "wav",
        "aac" => "aac",
        "flac" => "flac",
        "mp4" => "mp4",
        "ogg" => "ogg",
        "mkv" => "mkv",
        "mka" => "mka",
        "x-aac" => "x-aac",
        "m4a" => "m4a",
        "mpeg" => "mpeg",
        "mpga" => "mpga",
        "pcm" => "pcm",
        "webm" => "webm",
        _ => return None,
    })
}

fn document_filename(name: Option<&str>, media_type: &str) -> String {
    if let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    match converse_document_format(media_type) {
        Some(ext) => format!("document.{ext}"),
        None => "document".into(),
    }
}

fn document_ref_source(uri: String) -> IrDocumentSource {
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("s3://")
        || lower.starts_with("gs://")
    {
        IrDocumentSource::Url(uri)
    } else {
        IrDocumentSource::FileId(uri)
    }
}

fn decode_openai_file_part(part: &Value) -> Option<IrPart> {
    let file = part.get("file").unwrap_or(part);
    let name = str_field(file, "filename")
        .or_else(|| str_field(part, "filename"))
        .or_else(|| str_field(file, "name"))
        .or_else(|| str_field(part, "name"))
        .filter(|s| !s.is_empty());
    let media_type = str_field(file, "media_type")
        .or_else(|| str_field(part, "media_type"))
        .unwrap_or_default();
    if let Some(file_id) = str_field(file, "file_id").or_else(|| str_field(part, "file_id")) {
        return Some(IrPart::Document {
            source: IrDocumentSource::FileId(file_id),
            media_type,
            name,
        });
    }
    if let Some(url) = str_field(file, "file_url")
        .or_else(|| str_field(part, "file_url"))
        .or_else(|| str_field(file, "url"))
        .or_else(|| str_field(part, "url"))
    {
        return Some(IrPart::Document {
            source: document_ref_source(url),
            media_type,
            name,
        });
    }
    let data = str_field(file, "file_data")
        .or_else(|| str_field(part, "file_data"))
        .or_else(|| str_field(file, "data"))?;
    let (media_type, payload) = if let Some((mt, b64)) = split_data_url(&data) {
        (mt.to_string(), b64.to_string())
    } else {
        let media_type = if media_type.is_empty() {
            "application/pdf".into()
        } else {
            media_type
        };
        (media_type, data)
    };
    Some(IrPart::Document {
        source: IrDocumentSource::Base64(payload),
        media_type,
        name,
    })
}

fn decode_input_audio_part(part: &Value) -> Option<IrPart> {
    let audio = part.get("input_audio").unwrap_or(part);
    let data = str_field(audio, "data")?;
    let format = str_field(audio, "format").unwrap_or_else(|| "wav".into());
    Some(IrPart::Audio { data, format })
}

fn f32_field(value: &Value, key: &str) -> Option<f32> {
    value.get(key)?.as_f64().map(|n| n as f32)
}

fn i64_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64()
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
    use wiremux_auth::Wire;

    #[test]
    fn decode_non_object_names_the_wire_and_json_kind() {
        let err = decode(Wire::Messages, b"[1]").expect_err("array is not an object");
        let text = err.to_string();
        assert!(
            text.contains("wire `messages`") && text.contains("got array"),
            "{text}"
        );
    }

    #[test]
    fn decode_invalid_json_names_the_wire() {
        let err = decode(Wire::ChatCompletions, b"").expect_err("empty");
        let text = err.to_string();
        assert!(
            text.contains("wire `chat-completions`") && text.contains("invalid JSON"),
            "{text}"
        );
    }

    #[test]
    fn snake_and_kebab_leave_dots() {
        assert_eq!(convert_case("crm.lookupId", '_'), "crm.lookup_id");
        assert_eq!(convert_case("crm.lookup_id", '-'), "crm.lookup-id");
    }
}
