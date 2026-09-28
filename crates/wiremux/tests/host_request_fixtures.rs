//! Request fixtures Bline still asserts in host tests.
//! Each case is one [`IrRequest`] in and one JSON object out.
//! Effort budgets (24576, Messages 32768, and so on) are applied by the
//! host before encode. These tests send that IR.

use serde_json::{Value, json};
use wiremux::{
    IrCache, IrItem, IrPart, IrRequest, IrSampling, IrTool, ResolvedProfile, Wire, encode,
    parse_profile_str,
};

fn profile(wire: &str) -> ResolvedProfile {
    parse_profile_str(&format!(
        "schema_version = 1\nid = \"host-{wire}\"\nwire = \"{wire}\"\ntool_type_policy = \"hard-error\"\n"
    ))
    .expect("profile")
}

fn body(wire: Wire, ir: &IrRequest) -> Value {
    let (bytes, _) = encode(wire, ir, &profile(wire.as_str())).expect("encode");
    serde_json::from_slice(&bytes).expect("json")
}

fn user(text: &str) -> IrItem {
    IrItem::User {
        parts: vec![IrPart::Text(text.into())],
    }
}

fn weather_tool() -> IrTool {
    IrTool::Function {
        name: "get_weather".into(),
        description: "Get weather for a city".into(),
        parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    }
}

fn count_cache_control(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            let here = usize::from(map.contains_key("cache_control"));
            here + map.values().map(count_cache_control).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(count_cache_control).sum(),
        _ => 0,
    }
}

#[test]
fn gemini_system_is_instruction_and_roles_are_user_or_model() {
    let ir = IrRequest::new(
        "gemini-2.5-flash",
        vec![
            IrItem::System {
                text: "You are helpful.".into(),
            },
            user("Hi"),
            IrItem::Assistant {
                parts: vec![IrPart::Text("Hello!".into())],
            },
        ],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.temperature = Some(0.0);
        s.max_tokens = Some(8192);
    }));
    let json = body(Wire::Gemini, &ir);
    let sys = json.get("systemInstruction").expect("systemInstruction");
    assert!(
        sys.get("role").is_none(),
        "systemInstruction.role absent: {json}"
    );
    assert_eq!(sys["parts"][0]["text"], "You are helpful.");
    assert_eq!(json["contents"].as_array().unwrap().len(), 2);
    assert_eq!(json["contents"][0]["role"], "user");
    assert_eq!(json["contents"][1]["role"], "model");
    assert!(json.get("tools").is_none(), "no tools: {json}");
    assert_eq!(json["generationConfig"]["temperature"], json!(0.0));
    assert_eq!(json["generationConfig"]["maxOutputTokens"], 8192);
}

#[test]
fn gemini_function_tool_is_a_declaration_not_chat_function() {
    let ir =
        IrRequest::new("gemini-2.5-flash", vec![user("weather?")]).with_tools(vec![weather_tool()]);
    let json = body(Wire::Gemini, &ir);
    let decl = &json["tools"][0]["functionDeclarations"][0];
    assert_eq!(decl["name"], "get_weather");
    assert_eq!(decl["description"], "Get weather for a city");
    assert!(json["tools"][0].get("function").is_none(), "{json}");
}

#[test]
fn gemini_thinking_config_follows_host_visibility_and_budget() {
    let omitted = body(
        Wire::Gemini,
        &IrRequest::new("gemini-2.5-flash", vec![user("hi")]),
    );
    assert!(
        omitted
            .pointer("/generationConfig/thinkingConfig")
            .is_none(),
        "{omitted}"
    );

    let never = body(
        Wire::Gemini,
        &IrRequest::new("gemini-2.5-flash", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.include_thoughts = Some(false);
            },
        )),
    );
    let tc = never
        .pointer("/generationConfig/thinkingConfig")
        .expect("thinkingConfig");
    assert_eq!(tc["includeThoughts"], false);
    assert!(tc.get("thinkingBudget").is_none(), "{never}");

    let high = body(
        Wire::Gemini,
        &IrRequest::new("gemini-2.5-flash", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.include_thoughts = Some(true);
                s.thinking_budget = Some(24576);
            },
        )),
    );
    let tc = high.pointer("/generationConfig/thinkingConfig").unwrap();
    assert_eq!(tc["includeThoughts"], true);
    assert_eq!(tc["thinkingBudget"], 24576);

    let always = body(
        Wire::Gemini,
        &IrRequest::new("gemini-2.5-flash", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.include_thoughts = Some(true);
            },
        )),
    );
    let tc = always.pointer("/generationConfig/thinkingConfig").unwrap();
    assert_eq!(tc["includeThoughts"], true);
    assert!(tc.get("thinkingBudget").is_none(), "{always}");

    let capped = body(
        Wire::Gemini,
        &IrRequest::new("gemini-2.5-flash", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.include_thoughts = Some(true);
                s.reasoning_effort = Some("low".into());
                s.max_reasoning_tokens = Some(15000);
            },
        )),
    );
    assert_eq!(
        capped.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
        Some(&json!(15000))
    );
}

#[test]
fn gemini_images_and_thought_signatures_replay_on_the_next_request() {
    let images = body(
        Wire::Gemini,
        &IrRequest::new(
            "gemini-2.5-flash",
            vec![IrItem::User {
                parts: vec![
                    IrPart::ImageBase64 {
                        media_type: "image/png".into(),
                        data: "iVBORw0KGgo=".into(),
                    },
                    IrPart::ImageUrl("https://example.com/photo.png".into()),
                    IrPart::ImageUrl("see the notes".into()),
                ],
            }],
        ),
    );
    assert_eq!(
        images["contents"][0]["parts"][0]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        images["contents"][0]["parts"][0]["inlineData"]["data"],
        "iVBORw0KGgo="
    );
    // Bline `image_url_plain_becomes_text` keeps https images as text.
    // generateContent fileData is for documents, not this image part.
    assert_eq!(
        images["contents"][0]["parts"][1]["text"],
        "[image: https://example.com/photo.png]"
    );
    assert!(images["contents"][0]["parts"][1].get("fileData").is_none());
    assert_eq!(
        images["contents"][0]["parts"][2]["text"],
        "[image: see the notes]"
    );
    assert!(!images.to_string().contains("image_url"), "{images}");

    let signed = body(
        Wire::Gemini,
        &IrRequest::new(
            "gemini-2.5-flash",
            vec![
                user("list files"),
                IrItem::Assistant {
                    parts: vec![IrPart::Thinking {
                        text: "planning the listing".into(),
                        signature: Some("sig_thought_part".into()),
                    }],
                },
                IrItem::FunctionCall {
                    call_id: "call_1".into(),
                    name: "list_dir".into(),
                    arguments: r#"{"path":"."}"#.into(),
                    thought_signature: Some("sig_function_call".into()),
                },
                IrItem::FunctionCall {
                    call_id: "call_2".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"b.rs"}"#.into(),
                    thought_signature: None,
                },
            ],
        ),
    );
    let parts = signed["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["role"] == "model")
        .flat_map(|c| c["parts"].as_array().unwrap().clone())
        .collect::<Vec<_>>();
    let thought = parts
        .iter()
        .find(|p| p.get("functionCall").is_none())
        .unwrap();
    assert_eq!(thought["thoughtSignature"], "sig_thought_part");
    assert_eq!(thought["thought"], true);
    let list_dir = parts
        .iter()
        .find(|p| p["functionCall"]["name"] == "list_dir")
        .unwrap();
    assert_eq!(list_dir["thoughtSignature"], "sig_function_call");
    let write = parts
        .iter()
        .find(|p| p["functionCall"]["name"] == "write_file")
        .unwrap();
    assert!(write.get("thoughtSignature").is_none(), "{write}");

    let results = body(
        Wire::Gemini,
        &IrRequest::new(
            "gemini-2.5-flash",
            vec![
                IrItem::FunctionCall {
                    call_id: "fn_a".into(),
                    name: "fn_a".into(),
                    arguments: "{}".into(),
                    thought_signature: None,
                },
                IrItem::FunctionCall {
                    call_id: "fn_b".into(),
                    name: "fn_b".into(),
                    arguments: "{}".into(),
                    thought_signature: None,
                },
                IrItem::FunctionOutput {
                    call_id: "fn_a".into(),
                    output: r#"{"result":"a"}"#.into(),
                },
                IrItem::FunctionOutput {
                    call_id: "fn_b".into(),
                    output: r#"{"result":"b"}"#.into(),
                },
            ],
        ),
    );
    let user_turn = results["contents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["role"] == "user")
        .unwrap();
    let responses = user_turn["parts"].as_array().expect("parts");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["functionResponse"]["id"], "fn_a");
    assert_eq!(responses[0]["functionResponse"]["name"], "fn_a");
    assert_eq!(responses[0]["functionResponse"]["response"]["result"], "a");
    assert_eq!(responses[1]["functionResponse"]["id"], "fn_b");
    assert_eq!(responses[1]["functionResponse"]["name"], "fn_b");
    assert_eq!(responses[1]["functionResponse"]["response"]["result"], "b");
}

#[test]
fn chat_request_fixtures_match_host_encode() {
    let plain = IrRequest::new(
        "gpt-4o",
        vec![
            IrItem::System {
                text: "You are helpful.".into(),
            },
            user("Hi"),
        ],
    )
    .with_sampling(IrSampling::patch(|s| s.stream = Some(false)));
    let json = body(Wire::ChatCompletions, &plain);
    assert_eq!(json["messages"][0]["role"], "system");
    assert_eq!(json["messages"][1]["role"], "user");
    assert!(json.get("tools").is_none());
    assert_eq!(json["stream"], false);
    assert!(json.get("reasoning_effort").is_none(), "{json}");
    assert!(json.get("prompt_cache_key").is_none(), "{json}");

    let tooled = IrRequest::new("gpt-4o", vec![user("Hi")]).with_tools(vec![weather_tool()]);
    let json = body(Wire::ChatCompletions, &tooled);
    assert_eq!(json["tools"][0]["type"], "function");
    assert_eq!(json["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        json["tools"][0]["function"]["parameters"]["required"],
        json!([])
    );

    let streaming =
        IrRequest::new("gpt-4o", vec![user("Hi")]).with_sampling(IrSampling::patch(|s| {
            s.stream = Some(true);
        }));
    let json = body(Wire::ChatCompletions, &streaming);
    assert_eq!(json["stream"], true);
    assert_eq!(json["stream_options"]["include_usage"], true);

    for (effort, want) in [("high", "high"), ("low", "low"), ("xhigh", "xhigh")] {
        let ir = IrRequest::new("o3", vec![user("Hi")]).with_sampling(IrSampling::patch(|s| {
            s.reasoning_effort = Some(effort.into());
        }));
        assert_eq!(body(Wire::ChatCompletions, &ir)["reasoning_effort"], want);
    }

    let images = body(
        Wire::ChatCompletions,
        &IrRequest::new(
            "gpt-4o",
            vec![IrItem::User {
                parts: vec![
                    IrPart::ImageUrl("https://example.com/img.png".into()),
                    IrPart::ImageBase64 {
                        media_type: "image/jpeg".into(),
                        data: "/9j/4AAQ".into(),
                    },
                ],
            }],
        ),
    );
    let parts = images["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts[0]["type"], "image_url");
    assert_eq!(parts[0]["image_url"]["url"], "https://example.com/img.png");
    assert_eq!(
        parts[1]["image_url"]["url"],
        "data:image/jpeg;base64,/9j/4AAQ"
    );

    let tool = body(
        Wire::ChatCompletions,
        &IrRequest::new(
            "gpt-4o",
            vec![
                IrItem::Assistant {
                    parts: vec![
                        IrPart::Thinking {
                            text: "plan".into(),
                            signature: Some("sig".into()),
                        },
                        IrPart::Text("Let me check.".into()),
                    ],
                },
                IrItem::FunctionCall {
                    call_id: "call_abc".into(),
                    name: "get_weather".into(),
                    arguments: r#"{"city":"NYC"}"#.into(),
                    thought_signature: None,
                },
                IrItem::FunctionOutput {
                    call_id: "call_abc".into(),
                    output: r#"{"temp":72}"#.into(),
                },
            ],
        ),
    );
    assert_eq!(tool["messages"][0]["content"], "Let me check.");
    assert!(!tool.to_string().contains("plan"), "{tool}");
    let call = &tool["messages"][0]["tool_calls"][0];
    assert_eq!(call["id"], "call_abc");
    assert_eq!(call["function"]["name"], "get_weather");
    assert_eq!(call["function"]["arguments"], r#"{"city":"NYC"}"#);
    assert!(call["function"]["arguments"].is_string());
    assert_eq!(tool["messages"][1]["role"], "tool");
    assert_eq!(tool["messages"][1]["tool_call_id"], "call_abc");

    let keyed = IrRequest::new("grok-4", vec![user("Hi")]).with_sampling(IrSampling::patch(|s| {
        s.prompt_cache_key = Some("sess-1".into());
    }));
    assert_eq!(
        body(Wire::ChatCompletions, &keyed)["prompt_cache_key"],
        "sess-1"
    );
}

#[test]
fn responses_input_item_fixtures() {
    let tool = body(
        Wire::Responses,
        &IrRequest::new(
            "gpt-4o",
            vec![IrItem::FunctionOutput {
                call_id: "call_abc".into(),
                output: "sunny".into(),
            }],
        ),
    );
    let item = &tool["input"][0];
    assert_eq!(item["type"], "function_call_output");
    assert_eq!(item["call_id"], "call_abc");
    assert_eq!(item["output"], "sunny");

    let mixed = body(
        Wire::Responses,
        &IrRequest::new(
            "gpt-4o",
            vec![
                IrItem::Assistant {
                    parts: vec![IrPart::Text("checking".into())],
                },
                IrItem::FunctionCall {
                    call_id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: r#"{"city":"NYC"}"#.into(),
                    thought_signature: None,
                },
            ],
        ),
    );
    assert_eq!(mixed["input"][0]["role"], "assistant");
    assert_eq!(mixed["input"][1]["type"], "function_call");
    assert_eq!(mixed["input"][1]["name"], "get_weather");
    assert_eq!(mixed["input"][1]["call_id"], "call_1");
    assert_eq!(mixed["input"][1]["arguments"], r#"{"city":"NYC"}"#);

    let protocol = body(
        Wire::Responses,
        &IrRequest::new(
            "gpt-4o",
            vec![
                IrItem::HostedToolCall {
                    kind: "web_search_call".into(),
                    raw: json!({"type": "web_search_call", "id": "ws_1"}),
                },
                IrItem::Assistant {
                    parts: vec![IrPart::Text("done".into())],
                },
            ],
        ),
    );
    assert_eq!(protocol["input"][0]["id"], "ws_1");
    assert_eq!(protocol["input"][1]["role"], "assistant");

    let encrypted = body(
        Wire::Responses,
        &IrRequest::new(
            "gpt-4o",
            vec![IrItem::Assistant {
                parts: vec![
                    IrPart::Thinking {
                        text: "secret plan".into(),
                        signature: Some("enc_abc".into()),
                    },
                    IrPart::Text("Hello".into()),
                ],
            }],
        ),
    );
    let reasoning: Vec<_> = encrypted["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .collect();
    assert_eq!(reasoning.len(), 1, "{encrypted}");
    assert_eq!(reasoning[0]["encrypted_content"], "enc_abc");
    assert!(
        encrypted["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["role"] == "assistant")
    );

    let unsigned = body(
        Wire::Responses,
        &IrRequest::new(
            "gpt-4o",
            vec![IrItem::Assistant {
                parts: vec![
                    IrPart::Thinking {
                        text: "secret plan".into(),
                        signature: None,
                    },
                    IrPart::Text("Hello".into()),
                ],
            }],
        ),
    );
    assert!(
        unsigned["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] != "reasoning"),
        "{unsigned}"
    );
    assert_eq!(unsigned["input"][0]["role"], "assistant");

    let images = body(
        Wire::Responses,
        &IrRequest::new(
            "gpt-4o",
            vec![IrItem::User {
                parts: vec![
                    IrPart::Thinking {
                        text: "skip".into(),
                        signature: None,
                    },
                    IrPart::ImageUrl("https://example.com/img.png".into()),
                    IrPart::ImageBase64 {
                        media_type: "image/png".into(),
                        data: "iVBORw0KGgo=".into(),
                    },
                ],
            }],
        ),
    );
    let parts = images["input"][0]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "{images}");
    assert_eq!(parts[0]["image_url"], "https://example.com/img.png");
    assert_eq!(parts[1]["image_url"], "data:image/png;base64,iVBORw0KGgo=");
}

#[test]
fn messages_request_fixtures_match_host_encode() {
    let continued = body(
        Wire::Messages,
        &IrRequest::new(
            "claude-haiku-4-5",
            vec![IrItem::Assistant {
                parts: vec![IrPart::Text("done".into())],
            }],
        ),
    );
    let last = continued["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["text"], "Continue.");

    let tools = body(
        Wire::Messages,
        &IrRequest::new(
            "claude-haiku-4-5",
            vec![
                user("both"),
                IrItem::FunctionCall {
                    call_id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                    thought_signature: None,
                },
                IrItem::FunctionOutput {
                    call_id: "call_1".into(),
                    output: "sunny".into(),
                },
                IrItem::FunctionOutput {
                    call_id: "call_2".into(),
                    output: "cloudy".into(),
                },
            ],
        ),
    );
    let dumped = tools.to_string();
    assert!(
        dumped.contains("call_1") && dumped.contains("call_2"),
        "{tools}"
    );
    assert!(
        dumped.contains("sunny") && dumped.contains("cloudy"),
        "{tools}"
    );
    assert!(dumped.contains("get_weather"), "{tools}");

    let schema =
        IrRequest::new("claude-haiku-4-5", vec![user("hi")]).with_tools(vec![weather_tool()]);
    let json = body(Wire::Messages, &schema);
    assert_eq!(json["tools"][0]["input_schema"]["required"], json!([]));

    let replay = body(
        Wire::Messages,
        &IrRequest::new(
            "claude-haiku-4-5",
            vec![IrItem::Assistant {
                parts: vec![
                    IrPart::Thinking {
                        text: "plan".into(),
                        signature: Some("sig".into()),
                    },
                    IrPart::Text("   ".into()),
                    IrPart::Raw {
                        type_name: "redacted_thinking".into(),
                        raw: json!({"type": "redacted_thinking", "data": "enc"}),
                    },
                    IrPart::Text("Hello".into()),
                ],
            }],
        ),
    );
    let blocks = replay["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["signature"], "sig");
    assert!(blocks.iter().any(|b| b["type"] == "redacted_thinking"));
    assert!(blocks.iter().any(|b| b["text"] == "Hello"));
    assert!(blocks.iter().all(|b| b["text"] != "   "), "{replay}");

    let with_tool = body(
        Wire::Messages,
        &IrRequest::new(
            "claude-haiku-4-5",
            vec![
                IrItem::Assistant {
                    parts: vec![IrPart::Text(" \n".into())],
                },
                IrItem::FunctionCall {
                    call_id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                    thought_signature: None,
                },
            ],
        ),
    );
    let blocks = with_tool["messages"][0]["content"].as_array().unwrap();
    assert!(blocks.iter().all(|b| b["type"] != "text"), "{with_tool}");
    assert!(blocks.iter().any(|b| b["type"] == "tool_use"));

    let none = body(
        Wire::Messages,
        &IrRequest::new("claude-haiku-4-5", vec![user("hi")]),
    );
    assert!(none.get("thinking").is_none(), "{none}");

    let never = body(
        Wire::Messages,
        &IrRequest::new("claude-haiku-4-5", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.include_thoughts = Some(false);
                s.reasoning_effort = Some("high".into());
            },
        )),
    );
    assert_eq!(never["thinking"]["type"], "disabled");

    let high = body(
        Wire::Messages,
        &IrRequest::new("claude-haiku-4-5", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.reasoning_effort = Some("high".into());
            },
        )),
    );
    let budget = high["thinking"]["budget_tokens"].as_u64().unwrap();
    assert_eq!(budget, 32768);
    assert!(high["max_tokens"].as_u64().unwrap() > budget);

    let capped = body(
        Wire::Messages,
        &IrRequest::new("claude-haiku-4-5", vec![user("hi")]).with_sampling(IrSampling::patch(
            |s| {
                s.reasoning_effort = Some("low".into());
                s.max_reasoning_tokens = Some(15000);
            },
        )),
    );
    assert_eq!(capped["thinking"]["budget_tokens"], 15000);
    assert!(capped["max_tokens"].as_u64().unwrap() > 15000);

    for (effort, want) in [
        ("low", 4096),
        ("medium", 10240),
        ("high", 32768),
        ("xhigh", 65536),
    ] {
        let ir = IrRequest::new("claude-haiku-4-5", vec![user("hi")]).with_sampling(
            IrSampling::patch(|s| s.reasoning_effort = Some(effort.into())),
        );
        let json = body(Wire::Messages, &ir);
        let budget = json["thinking"]["budget_tokens"].as_u64().unwrap();
        assert_eq!(budget, want, "{effort}");
        assert!(
            json["max_tokens"].as_u64().unwrap() > budget,
            "{effort} {json}"
        );
    }
}

#[test]
fn messages_cache_breakpoints_follow_host_rules() {
    let off = IrRequest::new(
        "claude-haiku-4-5",
        vec![
            IrItem::System {
                text: "rules".into(),
            },
            user("hi"),
        ],
    );
    assert_eq!(count_cache_control(&body(Wire::Messages, &off)), 0);

    let none = off.clone().with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled().with_retention("none");
    }));
    assert_eq!(count_cache_control(&body(Wire::Messages, &none)), 0);

    let short = off.clone().with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled().with_retention("5m");
    }));
    let json = body(Wire::Messages, &short);
    let marker = json
        .pointer("/system/cache_control")
        .or_else(|| json.pointer("/system/0/cache_control"))
        .or_else(|| json.pointer("/messages/0/content/0/cache_control"))
        .expect("a short marker");
    assert_eq!(marker["type"], "ephemeral");
    assert!(marker.get("ttl").is_none(), "{json}");

    let floor = IrRequest::new(
        "claude-haiku-4-5",
        vec![IrItem::System { text: "sys".into() }, user("hi")],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled()
            .with_retention("1h")
            .with_min_cacheable_tokens(40);
    }));
    assert_eq!(count_cache_control(&body(Wire::Messages, &floor)), 0);

    let huge = IrRequest::new(
        "claude-haiku-4-5",
        vec![
            IrItem::System {
                text: "x".repeat(10_000),
            },
            IrItem::User {
                parts: vec![IrPart::Text("y".repeat(10_000))],
            },
        ],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled()
            .with_retention("1h")
            .with_min_cacheable_tokens(u32::MAX);
    }));
    assert_eq!(count_cache_control(&body(Wire::Messages, &huge)), 0);

    let small = IrRequest::new(
        "claude-haiku-4-5",
        vec![IrItem::System { text: "sys".into() }, user("hi")],
    )
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled()
            .with_retention("1h")
            .with_min_cacheable_tokens(40);
    }));
    let big_tool = IrTool::Function {
        name: "big_tool".into(),
        description: "x".repeat(400),
        parameters: json!({
            "type": "object",
            "properties": {"payload": {"type": "string", "description": "y".repeat(400)}}
        }),
    };
    assert_eq!(count_cache_control(&body(Wire::Messages, &small)), 0);
    let crossed = small.with_tools(vec![big_tool]);
    assert!(count_cache_control(&body(Wire::Messages, &crossed)) > 0);

    let mut items = Vec::new();
    for i in 0..6 {
        items.push(IrItem::System {
            text: format!("system fragment {i}"),
        });
    }
    items.push(user("do the work"));
    let six = IrRequest::new("claude-haiku-4-5", items).with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled().with_retention("1h");
    }));
    let json = body(Wire::Messages, &six);
    let system = json["system"].as_array().unwrap();
    assert_eq!(system.len(), 6);
    assert!(system[0]["cache_control"].is_object());
    for block in system.iter().skip(1) {
        assert!(block.get("cache_control").is_none(), "{json}");
    }
    assert_eq!(count_cache_control(&json), 2, "{json}");

    let paired = IrRequest::new(
        "claude-haiku-4-5",
        vec![
            IrItem::System {
                text: "You are helpful.".into(),
            },
            IrItem::System {
                text: "Restored goal".into(),
            },
            user("continue"),
        ],
    )
    .with_tools(vec![
        weather_tool(),
        IrTool::Function {
            name: "second_tool".into(),
            description: "second".into(),
            parameters: json!({"type": "object"}),
        },
    ])
    .with_sampling(IrSampling::patch(|s| {
        s.cache = IrCache::enabled().with_retention("1h");
    }));
    let json = body(Wire::Messages, &paired);
    assert!(json["tools"][0].get("cache_control").is_none());
    assert_eq!(json["tools"][1]["cache_control"]["ttl"], "1h");
    assert_eq!(json["system"][0]["cache_control"]["ttl"], "1h");
    assert!(json["system"][1].get("cache_control").is_none());
    assert_eq!(count_cache_control(&json), 2, "{json}");
}
