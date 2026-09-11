//! Namespace flatten, restore, and tool-type policy.

use serde_json::{Value, json};
use wiremux_auth::{ResolvedProfile, ToolNameCase, ToolTypePolicy, Wire};

use super::{MapError, apply_tool_name_case};
use crate::ir::{IrRequest, IrTool, LossAction, LossReport};

const HOSTED_KINDS: &[&str] = &[
    "web_search",
    "web_search_preview",
    "file_search",
    "code_interpreter",
    "computer",
    "image_generation",
    "mcp",
    "local_shell",
    "shell",
    "tool_search",
];

/// Tool ready for a dialect encoder.
#[derive(Clone, Debug)]
pub(super) enum PreparedTool {
    Function {
        name: String,
        description: String,
        parameters: Value,
    },
    Raw(Value),
}

pub(super) fn decode_tool(value: &Value) -> IrTool {
    let type_name = value.get("type").and_then(Value::as_str).unwrap_or("");
    match type_name {
        "namespace" => IrTool::Namespace {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            raw: value.clone(),
        },
        "function" | "" => decode_function_or_messages(value, type_name),
        other if is_hosted(other) => IrTool::Hosted {
            kind: other.to_string(),
            raw: value.clone(),
        },
        other => {
            if looks_like_function(value) {
                function_from(value)
            } else {
                IrTool::Unknown {
                    type_name: other.to_string(),
                    raw: value.clone(),
                }
            }
        }
    }
}

fn decode_function_or_messages(value: &Value, type_name: &str) -> IrTool {
    if let Some(func) = value.get("function") {
        return function_from(func);
    }
    if looks_like_function(value) {
        return function_from(value);
    }
    IrTool::Unknown {
        type_name: type_name.to_string(),
        raw: value.clone(),
    }
}

fn looks_like_function(value: &Value) -> bool {
    value.get("name").and_then(Value::as_str).is_some()
        && (value.get("parameters").is_some()
            || value.get("input_schema").is_some()
            || value.get("description").is_some()
            || value.get("type").and_then(Value::as_str) == Some("function"))
}

fn function_from(src: &Value) -> IrTool {
    IrTool::Function {
        name: src
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: src
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        parameters: src
            .get("parameters")
            .or_else(|| src.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
    }
}

fn is_hosted(type_name: &str) -> bool {
    HOSTED_KINDS.contains(&type_name)
}

pub(super) fn prepare_tools(
    wire: Wire,
    ir: &IrRequest,
    profile: &ResolvedProfile,
    report: &mut LossReport,
) -> Result<Vec<PreparedTool>, MapError> {
    let policy = profile.dialect.tool_type_policy;
    let case = profile
        .fingerprint
        .as_ref()
        .and_then(|fp| fp.tool_name_case);
    let has_namespace_slot = matches!(wire, Wire::Responses);
    let has_hosted_slot = matches!(wire, Wire::Responses);

    let mut out = Vec::new();
    for (idx, tool) in ir.tools.iter().enumerate() {
        let path = format!("tools[{idx}]");
        match tool {
            IrTool::Function {
                name,
                description,
                parameters,
            } => {
                out.push(PreparedTool::Function {
                    name: apply_tool_name_case(name, case),
                    description: description.clone(),
                    parameters: parameters.clone(),
                });
            }
            IrTool::Namespace { name, raw } => {
                push_namespace(
                    &mut out,
                    NsPush {
                        name,
                        raw,
                        path: &path,
                        policy,
                        has_namespace_slot,
                        case,
                        report,
                    },
                )?;
            }
            IrTool::Hosted { kind, raw } => {
                push_hosted(&mut out, kind, raw, &path, policy, has_hosted_slot, report)?;
            }
            IrTool::Unknown { type_name, raw } => match policy {
                ToolTypePolicy::Passthrough => {
                    report.record(&path, LossAction::Preserve, "unknown tool passthrough");
                    out.push(PreparedTool::Raw(raw.clone()));
                }
                ToolTypePolicy::HardError | ToolTypePolicy::FlattenNamespace => {
                    return Err(MapError::hard(
                        path,
                        format!("unknown tool type `{type_name}`"),
                    ));
                }
            },
        }
    }

    if matches!(policy, ToolTypePolicy::FlattenNamespace) && has_namespace_slot {
        out = restore_namespaces(out, report);
    }
    Ok(out)
}

struct NsPush<'a> {
    name: &'a str,
    raw: &'a Value,
    path: &'a str,
    policy: ToolTypePolicy,
    has_namespace_slot: bool,
    case: Option<ToolNameCase>,
    report: &'a mut LossReport,
}

fn push_namespace(out: &mut Vec<PreparedTool>, args: NsPush<'_>) -> Result<(), MapError> {
    let NsPush {
        name,
        raw,
        path,
        policy,
        has_namespace_slot,
        case,
        report,
    } = args;
    if has_namespace_slot && !matches!(policy, ToolTypePolicy::Passthrough) {
        report.record(path, LossAction::Preserve, "namespace tool");
        out.push(PreparedTool::Raw(namespace_raw(name, raw)));
        return Ok(());
    }
    match policy {
        ToolTypePolicy::Passthrough => {
            report.record(path, LossAction::Preserve, "namespace passthrough");
            out.push(PreparedTool::Raw(raw.clone()));
            Ok(())
        }
        ToolTypePolicy::FlattenNamespace => {
            let flat = flatten_namespace(name, raw, path)?;
            report.record(
                path,
                LossAction::Degrade,
                "flatten namespace to dotted function tools",
            );
            for tool in flat {
                if let IrTool::Function {
                    name,
                    description,
                    parameters,
                } = tool
                {
                    out.push(PreparedTool::Function {
                        name: apply_tool_name_case(&name, case),
                        description,
                        parameters,
                    });
                }
            }
            Ok(())
        }
        ToolTypePolicy::HardError => {
            // Silent strip is a bug. Flatten only when the profile opted in
            // and a nested tools table exists (handled above).
            Err(MapError::hard(
                path,
                "type=namespace is not silent-stripped; hard-error (set tool_type_policy = flatten-namespace to flatten)",
            ))
        }
    }
}

fn push_hosted(
    out: &mut Vec<PreparedTool>,
    kind: &str,
    raw: &Value,
    path: &str,
    policy: ToolTypePolicy,
    has_hosted_slot: bool,
    report: &mut LossReport,
) -> Result<(), MapError> {
    match policy {
        ToolTypePolicy::Passthrough => {
            report.record(path, LossAction::Preserve, "hosted passthrough");
            out.push(PreparedTool::Raw(raw.clone()));
            Ok(())
        }
        ToolTypePolicy::HardError | ToolTypePolicy::FlattenNamespace => {
            if has_hosted_slot {
                report.record(path, LossAction::Preserve, format!("hosted {kind}"));
                out.push(PreparedTool::Raw(raw.clone()));
                Ok(())
            } else {
                Err(MapError::hard(
                    path,
                    format!("hosted tool `{kind}` has no slot on this dialect"),
                ))
            }
        }
    }
}

fn namespace_raw(name: &str, raw: &Value) -> Value {
    let mut obj = match raw {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    obj.insert("type".into(), json!("namespace"));
    if !name.is_empty() {
        obj.insert("name".into(), json!(name));
    }
    Value::Object(obj)
}

fn flatten_namespace(name: &str, raw: &Value, path: &str) -> Result<Vec<IrTool>, MapError> {
    let Some(tools) = raw.get("tools").and_then(Value::as_array) else {
        return Err(MapError::hard(
            path,
            "namespace has no flatten table (tools[])",
        ));
    };
    let mut out = Vec::new();
    for child in tools {
        let child_name = child
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| MapError::hard(path, "namespace child missing name"))?;
        let dotted = format!("{name}.{child_name}");
        let description = child
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let parameters = child
            .get("parameters")
            .or_else(|| child.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        out.push(IrTool::Function {
            name: dotted,
            description,
            parameters,
        });
    }
    if out.is_empty() {
        return Err(MapError::hard(path, "namespace flatten table is empty"));
    }
    Ok(out)
}

fn restore_namespaces(tools: Vec<PreparedTool>, report: &mut LossReport) -> Vec<PreparedTool> {
    let mut out = Vec::new();
    let mut pending_ns: Option<String> = None;
    let mut pending_children: Vec<Value> = Vec::new();

    let flush = |out: &mut Vec<PreparedTool>,
                 pending_ns: &mut Option<String>,
                 pending_children: &mut Vec<Value>,
                 report: &mut LossReport| {
        let Some(ns) = pending_ns.take() else {
            return;
        };
        if pending_children.is_empty() {
            return;
        }
        report.record(
            format!("tools[{ns}]"),
            LossAction::Preserve,
            "restore namespace from dotted function names",
        );
        out.push(PreparedTool::Raw(json!({
            "type": "namespace",
            "name": ns,
            "description": "",
            "tools": pending_children.clone(),
        })));
        pending_children.clear();
    };

    for tool in tools {
        match tool {
            PreparedTool::Function {
                name,
                description,
                parameters,
            } => {
                if let Some((ns, leaf)) = split_namespace_name(&name) {
                    if pending_ns.as_deref() != Some(ns) {
                        flush(&mut out, &mut pending_ns, &mut pending_children, report);
                        pending_ns = Some(ns.to_string());
                    }
                    pending_children.push(json!({
                        "type": "function",
                        "name": leaf,
                        "description": description,
                        "parameters": parameters,
                    }));
                } else {
                    flush(&mut out, &mut pending_ns, &mut pending_children, report);
                    out.push(PreparedTool::Function {
                        name,
                        description,
                        parameters,
                    });
                }
            }
            other => {
                flush(&mut out, &mut pending_ns, &mut pending_children, report);
                out.push(other);
            }
        }
    }
    flush(&mut out, &mut pending_ns, &mut pending_children, report);
    out
}

pub(super) fn split_namespace_name(name: &str) -> Option<(&str, &str)> {
    let (ns, leaf) = name.split_once('.')?;
    if ns.is_empty() || leaf.is_empty() {
        return None;
    }
    Some((ns, leaf))
}

pub(super) fn qualify_call_name(item: &Value) -> String {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match item.get("namespace").and_then(Value::as_str) {
        Some(ns) if !ns.is_empty() && !name.starts_with(&format!("{ns}.")) => {
            format!("{ns}.{name}")
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrSampling;
    use wiremux_auth::parse_profile_str;

    fn profile(policy: &str) -> ResolvedProfile {
        parse_profile_str(&format!(
            r#"
schema_version = 1
id = "policy-test"
wire = "messages"
tool_type_policy = "{policy}"
"#
        ))
        .expect("profile")
    }

    fn ns_ir() -> IrRequest {
        IrRequest {
            model: "m".into(),
            items: Vec::new(),
            tools: vec![IrTool::Namespace {
                name: "crm".into(),
                raw: json!({
                    "type": "namespace",
                    "name": "crm",
                    "tools": [{
                        "type": "function",
                        "name": "lookup",
                        "description": "Lookup",
                        "parameters": {"type": "object", "properties": {}}
                    }]
                }),
            }],
            sampling: IrSampling::default(),
        }
    }

    #[test]
    fn hard_error_does_not_strip_namespace_on_messages() {
        let ir = ns_ir();
        let mut report = LossReport::default();
        let err = prepare_tools(Wire::Messages, &ir, &profile("hard-error"), &mut report)
            .expect_err("must hard-error");
        assert!(matches!(err, MapError::HardError { .. }));
        assert_eq!(ir.tools.len(), 1);
    }

    #[test]
    fn flatten_emits_dotted_function() {
        let ir = ns_ir();
        let mut report = LossReport::default();
        let tools = prepare_tools(
            Wire::Messages,
            &ir,
            &profile("flatten-namespace"),
            &mut report,
        )
        .expect("flatten");
        match &tools[0] {
            PreparedTool::Function {
                name, description, ..
            } => {
                assert_eq!(name, "crm.lookup");
                assert_eq!(description, "Lookup");
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn unknown_type_errors_under_hard_error() {
        let ir = IrRequest {
            model: "m".into(),
            items: Vec::new(),
            tools: vec![IrTool::Unknown {
                type_name: "weird".into(),
                raw: json!({"type": "weird"}),
            }],
            sampling: IrSampling::default(),
        };
        let mut report = LossReport::default();
        let err = prepare_tools(Wire::Responses, &ir, &profile("hard-error"), &mut report)
            .expect_err("unknown type");
        assert!(matches!(err, MapError::HardError { .. }));
    }
}
