use codex_plus_core::launcher::{DefaultLaunchHooks, LaunchHooks};
use codex_plus_core::protocol_proxy::{
    ChatSseToResponsesConverter, audio_transcriptions_url, chat_completion_to_response,
    chat_completion_to_response_with_request, chat_completions_url, chat_sse_to_responses_sse,
    chat_sse_to_responses_sse_with_request, is_audio_transcriptions_proxy_path,
    is_chat_completions_proxy_path, is_models_proxy_path, is_responses_compact_proxy_path,
    is_responses_proxy_path, models_url, open_audio_transcriptions_proxy_request,
    open_chat_completions_proxy_request, open_models_proxy_request, open_responses_proxy_request,
    open_responses_proxy_request_with_settings,
    open_responses_proxy_request_with_settings_for_path, responses_compact_url,
    responses_error_from_upstream, responses_to_chat_completions,
    restore_responses_tool_namespace_json, ResponsesNamespaceSseRewriter,
    send_upstream_request_with_header_timeout, upstream_header_timeout, upstream_http_client,
    upstream_stream_header_timeout, with_request_session_id,
};
use codex_plus_core::relay_config::test_relay_profile;
use codex_plus_core::relay_rotation::priority_fallback_cooldown_status;
use codex_plus_core::settings::{
    AggregateRelayMember, AggregateRelayProfile, AggregateRelayStrategy, BackendSettings,
    NativeAgentInterop, RelayMode, RelayModelRoute, RelayProfile, RelayProtocol,
    RelaySessionProvider, ResponsesReasoningPolicy,
};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn responses_request_converts_to_chat_completions() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "instructions": "You are helpful.",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "hello" }
                ]
            }
        ],
        "max_output_tokens": 512,
        "temperature": 0.2,
        "stream": true,
        "tools": [
            {
                "type": "function",
                "name": "lookup",
                "description": "Lookup data",
                "parameters": { "type": "object" }
            }
        ]
    }))
    .unwrap();

    assert_eq!(
        converted,
        json!({
            "model": "gpt-5-mini",
            "messages": [
                { "role": "system", "content": "You are helpful." },
                { "role": "user", "content": "hello" }
            ],
            "max_tokens": 512,
            "temperature": 0.2,
            "stream": true,
            "stream_options": { "include_usage": true },
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "description": "Lookup data",
                        "parameters": { "type": "object", "properties": {}, "required": [] }
                    }
                }
            ]
        })
    );
}

#[test]
fn responses_request_matches_ccs_reasoning_and_tool_choice_edges() {
    let non_reasoning = responses_to_chat_completions(json!({
        "model": "gpt-4o",
        "reasoning": { "effort": "high" },
        "tool_choice": { "type": "required" },
        "input": "hi"
    }))
    .unwrap();
    assert!(non_reasoning.get("reasoning_effort").is_none());
    assert!(non_reasoning.get("tool_choice").is_none());

    let reasoning = responses_to_chat_completions(json!({
        "model": "gpt-5.4",
        "reasoning": { "effort": "high" },
        "tool_choice": { "type": "function", "name": "lookup" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(reasoning["reasoning_effort"], "high");
    assert!(reasoning.get("tool_choice").is_none());

    let minimal = responses_to_chat_completions(json!({
        "model": "gpt-5.4",
        "reasoning": { "effort": "minimal" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(minimal["reasoning_effort"], "minimal");
}

#[test]
fn proxy_route_matchers_accept_ccswitch_codex_aliases() {
    for path in [
        "/responses",
        "/v1/responses",
        "/v1/v1/responses",
        "/codex/v1/responses",
        "/responses/compact",
        "/v1/responses/compact",
        "/v1/v1/responses/compact",
        "/codex/v1/responses/compact",
    ] {
        assert!(is_responses_proxy_path(path), "{path}");
    }
    assert!(is_responses_compact_proxy_path("/v1/responses/compact"));
    assert!(!is_responses_compact_proxy_path("/v1/responses"));

    for path in [
        "/chat/completions",
        "/v1/chat/completions",
        "/v1/v1/chat/completions",
        "/codex/v1/chat/completions",
    ] {
        assert!(is_chat_completions_proxy_path(path), "{path}");
    }

    for path in ["/models", "/v1/models", "/v1/v1/models", "/codex/v1/models"] {
        assert!(is_models_proxy_path(path), "{path}");
    }

    for path in [
        "/audio/transcriptions",
        "/v1/audio/transcriptions",
        "/v1/v1/audio/transcriptions",
        "/codex/v1/audio/transcriptions",
    ] {
        assert!(is_audio_transcriptions_proxy_path(path), "{path}");
    }
}

#[test]
fn responses_compact_url_preserves_compact_endpoint() {
    assert_eq!(
        responses_compact_url("https://api.example.test/v1"),
        "https://api.example.test/v1/responses/compact"
    );
    assert_eq!(
        responses_compact_url("https://api.example.test/v1/responses"),
        "https://api.example.test/v1/responses/compact"
    );
    assert_eq!(
        responses_compact_url("https://api.example.test/v1/responses/compact"),
        "https://api.example.test/v1/responses/compact"
    );
}

#[tokio::test]
async fn responses_compact_request_keeps_compact_path_upstream() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 35\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp_1\",\"object\":\"response\"}",
            )
            .await
            .unwrap();
        request
    });
    let settings = BackendSettings {
        active_relay_id: "compact".to_string(),
        relay_profiles: vec![RelayProfile {
            id: "compact".to_string(),
            name: "compact".to_string(),
            base_url: format!("http://{addr}/v1"),
            api_key: "sk-compact".to_string(),
            relay_mode: RelayMode::Official,
            official_mix_api_key: true,
            hide_official_usage_alert: false,
            ..RelayProfile::default()
        }],
        ..BackendSettings::default()
    };

    let result = open_responses_proxy_request_with_settings_for_path(
        r#"{"model":"gpt-5-mini","input":"hi","stream":false}"#,
        settings,
        "/v1/responses/compact",
    )
    .await
    .unwrap();
    let request = server.await.unwrap();

    assert_eq!(result.status_code, 200);
    assert!(request.starts_with("POST /v1/responses/compact HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-compact")
    );
}

#[test]
fn responses_request_applies_ccswitch_reasoning_dialects() {
    let deepseek = responses_to_chat_completions(json!({
        "model": "deepseek-reasoner",
        "reasoning": { "effort": "xhigh" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(deepseek["reasoning_effort"], "max");

    let openrouter = responses_to_chat_completions(json!({
        "model": "openrouter/deepseek/deepseek-r1",
        "reasoning": { "effort": "max" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(openrouter["reasoning"]["effort"], "xhigh");
    assert!(openrouter.get("reasoning_effort").is_none());

    let openrouter_off = responses_to_chat_completions(json!({
        "model": "openrouter/deepseek/deepseek-r1",
        "reasoning": { "effort": "none" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(openrouter_off["reasoning"]["effort"], "none");

    let kimi = responses_to_chat_completions(json!({
        "model": "kimi-k2-thinking",
        "reasoning": { "effort": "high" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(kimi["thinking"]["type"], "enabled");
    assert!(kimi.get("reasoning_effort").is_none());
}

#[test]
fn responses_request_maps_kimi_coding_reasoning_effort_per_official_spec() {
    // 官方映射 (kimi.com/code/docs): K3 接受 reasoning_effort low/high/max,
    // Codex 档位 minimal/low→low, medium/high→high, xhigh/max→max。
    for (effort, expected) in [
        ("minimal", "low"),
        ("low", "low"),
        ("medium", "high"),
        ("high", "high"),
        ("xhigh", "max"),
        ("max", "max"),
    ] {
        let converted = responses_to_chat_completions(json!({
            "model": "k3-256k",
            "reasoning": { "effort": effort },
            "input": "hi"
        }))
        .unwrap();
        assert_eq!(converted["thinking"]["type"], "enabled", "{effort}");
        assert_eq!(converted["reasoning_effort"], expected, "{effort}");
    }

    let k2_coding = responses_to_chat_completions(json!({
        "model": "kimi-for-coding",
        "reasoning": { "effort": "xhigh" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(k2_coding["reasoning_effort"], "max");

    // effort none → thinking disabled (官方: K3 关思考会被路由到 K2.6, 保持现状)
    let off = responses_to_chat_completions(json!({
        "model": "k3-256k",
        "reasoning": { "effort": "none" },
        "input": "hi"
    }))
    .unwrap();
    assert_eq!(off["thinking"]["type"], "disabled");
    assert!(off.get("reasoning_effort").is_none());
}

#[test]
fn responses_request_maps_developer_role_to_system_for_chat_upstream() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-chat",
        "input": [
            {
                "type": "message",
                "role": "developer",
                "content": [
                    { "type": "input_text", "text": "developer instructions" }
                ]
            },
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "hello" }
                ]
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "system");
    assert_eq!(
        converted["messages"][0]["content"],
        "developer instructions"
    );
    assert_eq!(converted["messages"][1]["role"], "user");
    assert!(
        !serde_json::to_string(&converted)
            .unwrap()
            .contains("\"developer\"")
    );
}

#[test]
fn responses_request_skips_additional_tools_without_content() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-chat",
        "instructions": "You are helpful.",
        "input": [
            {
                "type": "additional_tools",
                "role": "developer",
                "tools": [
                    { "type": "custom", "name": "exec", "description": "Run a command" }
                ]
            },
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "hello" }]
            }
        ]
    }))
    .unwrap();

    assert_eq!(
        converted["messages"],
        json!([
            { "role": "system", "content": "You are helpful." },
            { "role": "user", "content": "hello" }
        ])
    );
    assert!(
        converted["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| !message["content"].is_null())
    );
}

#[test]
fn responses_request_collapses_system_messages_to_head_for_strict_chat_upstreams() {
    let converted = responses_to_chat_completions(json!({
        "model": "MiniMax-M2.7",
        "instructions": "root system",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "hello" }]
            },
            {
                "type": "message",
                "role": "developer",
                "content": [{ "type": "input_text", "text": "late developer" }]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "ok" }]
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "system");
    assert_eq!(
        converted["messages"][0]["content"],
        "root system\n\nlate developer"
    );
    let system_count = converted["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .count();
    assert_eq!(system_count, 1);
    assert_eq!(converted["messages"][1]["role"], "user");
    assert_eq!(converted["messages"][2]["role"], "assistant");
}

#[test]
fn responses_request_maps_latest_reminder_to_user_like_ccswitch() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": [
            {
                "type": "message",
                "role": "latest_reminder",
                "content": [
                    { "type": "input_text", "text": "remember this" }
                ]
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "user");
    assert_eq!(converted["messages"][0]["content"], "remember this");
}

#[test]
fn responses_request_preserves_reasoning_content_for_thinking_followup() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-reasoner",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "use the tool" }]
            },
            {
                "id": "rs_1",
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": "Need to inspect files." }]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"rg foo\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "result"
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][1]["role"], "assistant");
    assert_eq!(
        converted["messages"][1]["reasoning_content"],
        "Need to inspect files."
    );
    assert_eq!(converted["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(converted["messages"][2]["role"], "tool");
}

// #1860 错误 1：孤立的 function_call（后面没有 function_call_output）不能变成
// assistant.tool_calls，否则 DeepSeek 报 "must be followed by tool messages"。
#[test]
fn responses_request_drops_orphaned_function_call_without_output() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash",
        "stream": false,
        "max_output_tokens": 8,
        "input": [
            { "role": "user", "content": "ping" },
            { "role": "assistant", "content": [{ "type": "output_text", "text": "ok" }] },
            {
                "type": "function_call",
                "call_id": "call_t1",
                "name": "shell_command",
                "arguments": "{\"command\":\"echo hi\"}"
            },
            { "role": "user", "content": "ping" }
        ]
    }))
    .unwrap();

    let messages = converted["messages"].as_array().unwrap();
    for (index, message) in messages.iter().enumerate() {
        let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) else {
            continue;
        };
        // 每个 tool_call 后面都必须紧跟对应的 tool 消息
        for (offset, tool_call) in tool_calls.iter().enumerate() {
            let id = tool_call["id"].as_str().unwrap();
            let follower = messages.get(index + 1 + offset);
            assert_eq!(
                follower
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str()),
                Some("tool"),
                "tool_call {id} 后面没有 tool 消息：{converted:#}"
            );
            assert_eq!(
                follower
                    .and_then(|m| m.get("tool_call_id"))
                    .and_then(|r| r.as_str()),
                Some(id),
                "tool_call {id} 没有匹配的 tool_call_id：{converted:#}"
            );
        }
    }
}

// #1860 错误 2：thinking 模式下带 tool_calls 的 assistant 消息必须回传 reasoning_content。
#[test]
fn responses_request_attaches_reasoning_content_to_tool_call_message() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash",
        "stream": false,
        "max_output_tokens": 8,
        "input": [
            { "role": "user", "content": "ping" },
            {
                "type": "function_call",
                "call_id": "call_t1",
                "name": "shell_command",
                "arguments": "{\"command\":\"echo hi\"}"
            },
            { "type": "function_call_output", "call_id": "call_t1", "output": "hi" }
        ]
    }))
    .unwrap();

    let messages = converted["messages"].as_array().unwrap();
    let tool_call_message = messages
        .iter()
        .find(|m| m.get("tool_calls").is_some())
        .unwrap_or_else(|| panic!("没有 tool_calls 消息：{converted:#}"));
    let has_content = tool_call_message
        .get("content")
        .and_then(|c| c.as_str())
        .is_some_and(|c| !c.is_empty());
    let has_reasoning = tool_call_message
        .get("reasoning_content")
        .and_then(|c| c.as_str())
        .is_some_and(|c| !c.is_empty());
    assert!(
        has_content || has_reasoning,
        "带 tool_calls 且 content 为空的 assistant 消息必须有 reasoning_content：{converted:#}"
    );
}

// 历史尾部的 tool_call 是「output 还没回来」的正常形态，必须保留。
#[test]
fn responses_request_keeps_trailing_unanswered_tool_call() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash",
        "input": [
            { "role": "user", "content": "ping" },
            {
                "type": "function_call",
                "call_id": "call_tail",
                "name": "shell_command",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        ]
    }))
    .unwrap();

    let last = converted["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["tool_calls"][0]["id"], "call_tail");
}

// 部分应答：只摘掉没被应答的那个，已应答的保留。
#[test]
fn responses_request_strips_only_unanswered_parallel_tool_calls() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash",
        "input": [
            { "role": "user", "content": "ping" },
            {
                "type": "function_call",
                "call_id": "call_ok",
                "name": "answered_tool",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "call_id": "call_lost",
                "name": "abandoned_tool",
                "arguments": "{}"
            },
            { "type": "function_call_output", "call_id": "call_ok", "output": "done" },
            { "role": "user", "content": "continue" }
        ]
    }))
    .unwrap();

    let assistant = converted["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message.get("tool_calls").is_some())
        .unwrap();
    let calls = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "只应保留被应答的 tool_call：{converted:#}");
    assert_eq!(calls[0]["id"], "call_ok");
    // 被摘掉的调用降级成文本保留，不静默丢失
    let content = assistant["content"].as_str().unwrap();
    assert!(
        content.contains("call_lost") && content.contains("abandoned_tool"),
        "被摘掉的调用应降级为文本：{content}"
    );
}

// 已有真实 reasoning 时不能被占位文本覆盖。
#[test]
fn responses_request_keeps_real_reasoning_content_over_placeholder() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash",
        "input": [
            { "role": "user", "content": "ping" },
            {
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": "Need to run echo." }]
            },
            {
                "type": "function_call",
                "call_id": "call_t1",
                "name": "shell_command",
                "arguments": "{}"
            },
            { "type": "function_call_output", "call_id": "call_t1", "output": "hi" }
        ]
    }))
    .unwrap();

    let assistant = converted["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message.get("tool_calls").is_some())
        .unwrap();
    assert_eq!(assistant["reasoning_content"], "Need to run echo.");
}

#[test]
fn responses_request_merges_reasoning_text_and_tool_calls_like_ccx() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-pro",
        "input": [
            {
                "type": "reasoning",
                "status": "completed",
                "summary": [{ "type": "summary_text", "text": "I need to run go vet." }]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Let me run go vet." }]
            },
            {
                "type": "function_call",
                "call_id": "call_001",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"go vet ./...\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_001",
                "output": "no issues found"
            },
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "run tests now" }]
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "assistant");
    assert_eq!(converted["messages"][0]["content"], "Let me run go vet.");
    assert_eq!(
        converted["messages"][0]["reasoning_content"],
        "I need to run go vet."
    );
    assert_eq!(converted["messages"][0]["tool_calls"][0]["id"], "call_001");
    assert_eq!(converted["messages"][1]["role"], "tool");
    assert_eq!(converted["messages"][1]["tool_call_id"], "call_001");
    assert_eq!(converted["messages"][2]["role"], "user");
}

#[test]
fn responses_request_normalizes_empty_assistant_messages_for_chat_upstream() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-chat",
        "input": [
            {
                "type": "message",
                "role": "assistant",
                "content": null
            },
            {
                "type": "message",
                "role": "assistant",
                "content": []
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "assistant");
    assert_eq!(converted["messages"][0]["content"], "");
    assert_eq!(converted["messages"][1]["role"], "assistant");
    assert_eq!(converted["messages"][1]["content"], "");
}

#[test]
fn responses_request_drops_tool_controls_when_no_chat_tools_survive() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": "hi",
        "tools": [
            { "type": "unknown_builtin", "name": "unsupported" }
        ],
        "tool_choice": { "type": "required" },
        "parallel_tool_calls": true
    }))
    .unwrap();

    assert!(converted.get("tools").is_none());
    assert!(converted.get("tool_choice").is_none());
    assert!(converted.get("parallel_tool_calls").is_none());
}

#[test]
fn responses_request_normalizes_function_tool_parameters() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": "hi",
        "tools": [
            {
                "type": "function",
                "name": "lookup",
                "parameters": {}
            }
        ]
    }))
    .unwrap();

    let params = &converted["tools"][0]["function"]["parameters"];
    assert_eq!(params["type"], "object");
    assert_eq!(params["properties"], json!({}));
    assert_eq!(params["required"], json!([]));
}

#[test]
fn responses_request_maps_codex_custom_and_namespace_tools_to_chat_functions() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": "hi",
        "tools": [
            {
                "type": "custom",
                "name": "exec",
                "description": "Run a command"
            },
            {
                "type": "namespace",
                "name": "mcp__vscode_mcp__",
                "description": "VS Code MCP",
                "tools": [
                    {
                        "type": "function",
                        "name": "open_file",
                        "description": "Open a file",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" }
                            },
                            "required": ["path"]
                        }
                    }
                ]
            },
            {
                "type": "web_search"
            }
        ],
        "tool_choice": {
            "type": "function",
            "namespace": "mcp__vscode_mcp__",
            "name": "open_file"
        },
        "parallel_tool_calls": true
    }))
    .unwrap();

    let names: Vec<_> = converted["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"exec"));
    assert!(names.contains(&"mcp__vscode_mcp__open_file"));
    assert!(names.contains(&"web_search"));
    assert_eq!(
        converted["tools"][0]["function"]["parameters"]["properties"]["input"]["type"],
        "string"
    );
    assert_eq!(converted["parallel_tool_calls"], true);
    assert_eq!(
        converted["tool_choice"]["function"]["name"],
        "mcp__vscode_mcp__open_file"
    );
}

#[test]
fn responses_request_stream_includes_usage_and_apply_patch_proxy_tools() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": "hi",
        "stream": true,
        "tools": [
            {
                "type": "custom",
                "name": "apply_patch",
                "description": "Patch files"
            }
        ],
        "tool_choice": { "type": "custom", "name": "apply_patch" }
    }))
    .unwrap();

    assert_eq!(converted["stream_options"]["include_usage"], true);
    let names: Vec<_> = converted["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "apply_patch_add_file",
            "apply_patch_delete_file",
            "apply_patch_update_file",
            "apply_patch_replace_file",
            "apply_patch_batch"
        ]
    );
    assert_eq!(
        converted["tools"][2]["function"]["parameters"]["properties"]["hunks"]["items"]["properties"]
            ["lines"]["items"]["required"],
        json!(["op", "text"])
    );
    assert_eq!(
        converted["tool_choice"]["function"]["name"],
        "apply_patch_batch"
    );
}

#[test]
fn responses_input_replays_custom_and_legacy_tool_history() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": [
            {
                "type": "custom_tool_call",
                "call_id": "call_custom",
                "name": "exec",
                "input": "ls -la"
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "call_custom",
                "output": "ok"
            },
            {
                "type": "tool_call",
                "tool_use": {
                    "id": "call_legacy",
                    "name": "lookup",
                    "input": { "query": "rust" }
                }
            },
            {
                "type": "tool_result",
                "content": {
                    "tool_use_id": "call_legacy",
                    "content": { "result": "found" }
                }
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "assistant");
    assert_eq!(
        converted["messages"][0]["tool_calls"][0]["id"],
        "call_custom"
    );
    assert_eq!(
        converted["messages"][0]["tool_calls"][0]["function"]["name"],
        "exec"
    );
    assert_eq!(
        converted["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{\"input\":\"ls -la\"}"
    );
    assert_eq!(converted["messages"][1]["role"], "tool");
    assert_eq!(converted["messages"][1]["content"], "ok");
    assert_eq!(
        converted["messages"][2]["tool_calls"][0]["id"],
        "call_legacy"
    );
    assert_eq!(
        converted["messages"][3]["content"],
        "{\"result\":\"found\"}"
    );
}

#[test]
fn responses_input_flattens_namespace_function_history_and_skips_invalid_tool_items() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": [
            {
                "type": "function_call",
                "call_id": "call_ns",
                "namespace": "mcp__vscode_mcp__",
                "name": "execute_command",
                "arguments": "{\"command\":\"save\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_ns",
                "output": "saved"
            },
            {
                "type": "function_call",
                "call_id": "missing_name",
                "arguments": "{}"
            },
            {
                "type": "function_call_output",
                "output": "orphan"
            }
        ]
    }))
    .unwrap();

    assert_eq!(
        converted["messages"][0]["tool_calls"][0]["function"]["name"],
        "mcp__vscode_mcp__execute_command"
    );
    assert_eq!(converted["messages"][1]["tool_call_id"], "call_ns");
    assert_eq!(converted["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn responses_input_sanitizes_invalid_function_call_arguments_history() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": [
            {
                "type": "function_call",
                "call_id": "bad_object",
                "name": "broken_args",
                "arguments": "{foo: \"bar\"}"
            },
            {
                "type": "function_call",
                "call_id": "plain_text",
                "name": "plain_args",
                "arguments": "raw text with \"quotes\" and \\slashes"
            },
            {
                "type": "function_call",
                "call_id": "array_args",
                "name": "array_args",
                "arguments": "[1,2,3]"
            },
            {
                "type": "tool_call",
                "tool_use": {
                    "id": "object_args",
                    "name": "object_args",
                    "input": { "ok": true }
                }
            }
        ]
    }))
    .unwrap();

    let calls = converted["messages"][0]["tool_calls"].as_array().unwrap();
    for call in calls {
        let arguments = call["function"]["arguments"].as_str().unwrap();
        serde_json::from_str::<serde_json::Value>(arguments)
            .expect("chat tool call arguments must always be valid JSON");
    }
    assert_eq!(
        calls[0]["function"]["arguments"],
        "{\"input\":\"{foo: \\\"bar\\\"}\"}"
    );
    assert_eq!(
        calls[1]["function"]["arguments"],
        "{\"input\":\"raw text with \\\"quotes\\\" and \\\\slashes\"}"
    );
    assert_eq!(calls[2]["function"]["arguments"], "{\"input\":[1,2,3]}");
    assert_eq!(calls[3]["function"]["arguments"], "{\"ok\":true}");
}

#[test]
fn responses_input_downgrades_orphan_tool_outputs_to_user_messages() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": [
            {
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": "I need the previous tool result." }]
            },
            {
                "type": "function_call_output",
                "call_id": "missing_call",
                "output": "tool output without a matching call"
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "missing_custom",
                "output": "custom output without a matching call"
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][0]["role"], "assistant");
    assert!(converted["messages"][0].get("tool_calls").is_none());
    assert_eq!(converted["messages"][1]["role"], "user");
    assert_eq!(
        converted["messages"][1]["content"],
        "Function call output (missing_call): tool output without a matching call"
    );
    assert_eq!(converted["messages"][2]["role"], "user");
    assert_eq!(
        converted["messages"][2]["content"],
        "Function call output (missing_custom): custom output without a matching call"
    );
}

#[test]
fn responses_input_replays_apply_patch_custom_history_as_proxy_tool() {
    let converted = responses_to_chat_completions(json!({
        "model": "gpt-5-mini",
        "input": [
            {
                "type": "custom_tool_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Add File: docs/test.md\n+# Test\n*** End Patch"
            }
        ],
        "tools": [{ "type": "custom", "name": "apply_patch" }]
    }))
    .unwrap();

    assert_eq!(
        converted["messages"][0]["tool_calls"][0]["function"]["name"],
        "apply_patch_add_file"
    );
    assert_eq!(
        converted["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{\"content\":\"# Test\",\"path\":\"docs/test.md\"}"
    );
}

#[test]
fn upstream_chat_error_is_regularized_as_responses_error_envelope() {
    let json_error = responses_error_from_upstream(
        400,
        "application/json",
        br#"{"error":{"message":"bad request","type":"invalid_request_error","code":"bad_model","param":"model"}}"#,
    );
    assert_eq!(json_error["error"]["message"], "bad request");
    assert_eq!(json_error["error"]["type"], "invalid_request_error");
    assert_eq!(json_error["error"]["code"], "bad_model");
    assert_eq!(json_error["error"]["param"], "model");

    let text_error = responses_error_from_upstream(502, "text/html", b"<html>bad gateway</html>");
    assert_eq!(text_error["error"]["message"], "<html>bad gateway</html>");
    assert_eq!(text_error["error"]["type"], "upstream_error");
    assert_eq!(text_error["error"]["code"], "502");
}

#[test]
fn chat_completion_response_converts_to_responses_response() {
    let converted = chat_completion_to_response(json!({
        "id": "chatcmpl_123",
        "created": 1710000000,
        "model": "gpt-5-mini",
        "choices": [
            {
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "hi there"
                }
            }
        ],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    }))
    .unwrap();

    assert_eq!(converted["object"], "response");
    assert_eq!(converted["status"], "completed");
    assert_eq!(converted["model"], "gpt-5-mini");
    assert_eq!(converted["usage"]["input_tokens"], 10);
    assert_eq!(converted["usage"]["output_tokens"], 5);
    assert_eq!(converted["output"][0]["type"], "message");
    assert_eq!(converted["output"][0]["content"][0]["text"], "hi there");
}

#[test]
fn chat_completion_response_maps_reasoning_tool_calls_and_usage_details() {
    let converted = chat_completion_to_response(json!({
        "id": "chatcmpl_1",
        "created": 123,
        "model": "gpt-5.4",
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "reasoning_content": "I should check first.",
                "content": "Let me check.",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Tokyo\"}"
                    }
                }]
            }
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "prompt_tokens_details": { "cached_tokens": 3 },
            "completion_tokens_details": { "reasoning_tokens": 2 }
        }
    }))
    .unwrap();

    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(
        converted["output"][0]["summary"][0]["text"],
        "I should check first."
    );
    assert_eq!(
        converted["output"][0]["reasoning_content"],
        "I should check first."
    );
    assert_eq!(converted["output"][1]["type"], "message");
    assert_eq!(converted["output"][2]["type"], "function_call");
    assert_eq!(converted["output"][2]["call_id"], "call_1");
    assert_eq!(
        converted["usage"]["input_tokens_details"]["cached_tokens"],
        3
    );
    assert_eq!(
        converted["usage"]["output_tokens_details"]["reasoning_tokens"],
        2
    );
}

#[test]
fn chat_completion_response_defaults_missing_reasoning_tokens_to_zero() {
    // Kimi 等上游在一次响应无 reasoning 时会省略 completion_tokens_details
    // 里的 reasoning_tokens; Codex 将该字段当必填解析, 缺省会报
    // "missing field `reasoning_tokens`" 并把整轮判为断流。
    let converted = chat_completion_to_response(json!({
        "id": "chatcmpl_no_reasoning",
        "created": 123,
        "model": "k3-256k",
        "choices": [{
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": "done" }
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "completion_tokens_details": {}
        }
    }))
    .unwrap();
    assert_eq!(
        converted["usage"]["output_tokens_details"]["reasoning_tokens"],
        0
    );
}

#[test]
fn chat_sse_defaults_missing_reasoning_tokens_to_zero() {
    let sse = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_kimi","created":123,"model":"k3-256k","choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":6,"total_tokens":10,"completion_tokens_details":{}}}

data: [DONE]

"#,
    );
    assert!(sse.contains("event: response.completed"));
    assert!(sse.contains("\"reasoning_tokens\":0"));
}

#[test]
fn chat_sse_without_usage_details_still_emits_reasoning_tokens() {
    // 上游连 completion_tokens_details 都没有时也要补上, Codex 才能解析。
    let sse = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_plain","created":123,"model":"k3-256k","choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":6,"total_tokens":10}}

data: [DONE]

"#,
    );
    assert!(sse.contains("event: response.completed"));
    assert!(sse.contains("\"output_tokens_details\":{\"reasoning_tokens\":0}"));
}

#[test]
fn chat_sse_without_any_usage_still_emits_reasoning_tokens() {
    // 上游全程未发 usage chunk → default usage 兜底同样带齐结构。
    let sse = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_nousg","created":123,"model":"k3-256k","choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}]}

data: [DONE]

"#,
    );
    assert!(sse.contains("event: response.completed"));
    assert!(sse.contains("\"reasoning_tokens\":0"));
}

#[test]
fn chat_completion_response_extracts_reasoning_details_like_ccswitch() {
    let converted = chat_completion_to_response(json!({
        "id": "chatcmpl_reasoning_details",
        "created": 123,
        "model": "MiniMax-M2.7",
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "reasoning_details": [
                    { "summary": "Step one." },
                    { "parts": [{ "text": "Step two." }] }
                ],
                "content": "final"
            }
        }]
    }))
    .unwrap();

    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(
        converted["output"][0]["summary"][0]["text"],
        "Step one.\n\nStep two."
    );
    assert_eq!(converted["output"][1]["content"][0]["text"], "final");
}

#[test]
fn chat_completion_response_accepts_responses_style_usage_fields() {
    let converted = chat_completion_to_response(json!({
        "id": "chatcmpl_usage",
        "created": 123,
        "model": "gpt-5.4",
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "ok"
            }
        }],
        "usage": {
            "input_tokens": 7,
            "output_tokens": 3,
            "input_tokens_details": { "cached_tokens": 2 },
            "cache_read_input_tokens": 1,
            "cache_creation_input_tokens": 4
        }
    }))
    .unwrap();

    assert_eq!(converted["usage"]["input_tokens"], 7);
    assert_eq!(converted["usage"]["output_tokens"], 3);
    assert_eq!(converted["usage"]["total_tokens"], 15);
    assert!(converted["usage"].get("input_tokens_details").is_none());
    assert_eq!(converted["usage"]["cache_read_input_tokens"], 1);
    assert_eq!(converted["usage"]["cache_creation_input_tokens"], 4);
}

#[test]
fn chat_completion_response_maps_custom_and_namespace_calls_with_request_context() {
    let request = json!({
        "model": "gpt-5-mini",
        "input": "hi",
        "tools": [
            { "type": "custom", "name": "exec" },
            {
                "type": "namespace",
                "name": "mcp__vscode_mcp__",
                "tools": [
                    { "type": "function", "name": "open_file", "parameters": {} }
                ]
            }
        ]
    });
    let converted = chat_completion_to_response_with_request(
        json!({
            "id": "chatcmpl_tools",
            "created": 123,
            "model": "gpt-5-mini",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call_custom",
                            "type": "function",
                            "function": {
                                "name": "exec",
                                "arguments": "{\"input\":\"ls -la\"}"
                            }
                        },
                        {
                            "id": "call_ns",
                            "type": "function",
                            "function": {
                                "name": "mcp__vscode_mcp__open_file",
                                "arguments": "{\"path\":\"src/main.rs\"}"
                            }
                        }
                    ]
                }
            }]
        }),
        &request,
    )
    .unwrap();

    assert_eq!(converted["output"][0]["type"], "custom_tool_call");
    assert_eq!(converted["output"][0]["name"], "exec");
    assert_eq!(converted["output"][0]["input"], "ls -la");
    assert_eq!(converted["output"][1]["type"], "function_call");
    assert_eq!(converted["output"][1]["name"], "open_file");
    assert_eq!(converted["output"][1]["namespace"], "mcp__vscode_mcp__");
}

#[test]
fn chat_completion_response_reconstructs_apply_patch_proxy_call() {
    let converted = chat_completion_to_response_with_request(
        json!({
            "id": "chatcmpl_patch",
            "created": 123,
            "model": "gpt-5-mini",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch_add_file",
                            "arguments": "{\"path\":\"README.md\",\"content\":\"hello\"}"
                        }
                    }]
                }
            }]
        }),
        &json!({
            "model": "gpt-5-mini",
            "tools": [{ "type": "custom", "name": "apply_patch" }]
        }),
    )
    .unwrap();

    assert_eq!(converted["output"][0]["type"], "custom_tool_call");
    assert_eq!(converted["output"][0]["name"], "apply_patch");
    assert_eq!(
        converted["output"][0]["input"],
        "*** Begin Patch\n*** Add File: README.md\n+hello\n*** End Patch"
    );
}

#[test]
fn chat_completion_response_remaps_string_apply_patch_proxy_tools() {
    let converted = chat_completion_to_response_with_request(
        json!({
            "id": "chatcmpl_patch_string_tool",
            "created": 123,
            "model": "gpt-5-mini",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch_add_file",
                            "arguments": "{\"path\":\"docs/test.md\",\"content\":\"# Test\\n\"}"
                        }
                    }]
                }
            }]
        }),
        &json!({
            "model": "gpt-5-mini",
            "tools": ["apply_patch_add_file", "apply_patch_batch"]
        }),
    )
    .unwrap();

    assert_eq!(converted["output"][0]["type"], "custom_tool_call");
    assert_eq!(converted["output"][0]["name"], "apply_patch");
    assert_eq!(
        converted["output"][0]["input"],
        "*** Begin Patch\n*** Add File: docs/test.md\n+# Test\n*** End Patch"
    );
}

#[test]
fn chat_completion_response_maps_gemini_and_claude_cache_usage_like_ccx() {
    let gemini = chat_completion_to_response(json!({
        "id": "chatcmpl_gemini_usage",
        "created": 123,
        "model": "gemini-proxy",
        "choices": [{ "finish_reason": "stop", "message": { "role": "assistant", "content": "ok" } }],
        "usage": {
            "promptTokenCount": 20,
            "cachedContentTokenCount": 5,
            "candidatesTokenCount": 7
        }
    }))
    .unwrap();
    assert_eq!(gemini["usage"]["input_tokens"], 15);
    assert_eq!(gemini["usage"]["output_tokens"], 7);
    assert_eq!(gemini["usage"]["total_tokens"], 27);
    assert_eq!(gemini["usage"]["input_tokens_details"]["cached_tokens"], 5);

    let claude = chat_completion_to_response(json!({
        "id": "chatcmpl_claude_usage",
        "created": 123,
        "model": "claude-proxy",
        "choices": [{ "finish_reason": "stop", "message": { "role": "assistant", "content": "ok" } }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3,
            "cache_read_input_tokens": 2,
            "cache_creation_5m_input_tokens": 4,
            "cache_creation_1h_input_tokens": 6
        }
    }))
    .unwrap();
    assert_eq!(claude["usage"]["input_tokens"], 10);
    assert_eq!(claude["usage"]["total_tokens"], 25);
    assert_eq!(claude["usage"]["cache_read_input_tokens"], 2);
    assert_eq!(claude["usage"]["cache_creation_5m_input_tokens"], 4);
    assert_eq!(claude["usage"]["cache_creation_1h_input_tokens"], 6);
    assert_eq!(claude["usage"]["cache_ttl"], "mixed");
    assert!(claude["usage"].get("input_tokens_details").is_none());
}

#[test]
fn chat_completion_response_splits_inline_think_block() {
    let converted = chat_completion_to_response(json!({
        "id": "chatcmpl_think",
        "created": 123,
        "model": "MiniMax-M2.7",
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "<think>\nNeed context.\n</think>\n\npong"
            }
        }]
    }))
    .unwrap();

    assert_eq!(converted["output"][0]["type"], "reasoning");
    assert_eq!(
        converted["output"][0]["summary"][0]["text"],
        "Need context."
    );
    assert_eq!(converted["output"][1]["type"], "message");
    assert_eq!(converted["output"][1]["content"][0]["text"], "pong");
}

#[test]
fn chat_sse_converts_to_responses_sse_events() {
    let converted = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_1","created":1710000000,"model":"gpt-5-mini","choices":[{"delta":{"content":"hel"},"finish_reason":null}]}

data: {"id":"chatcmpl_1","created":1710000000,"model":"gpt-5-mini","choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}

data: [DONE]

"#,
    );

    assert!(converted.contains("event: response.created"));
    assert!(converted.contains("event: response.output_text.delta"));
    assert!(converted.contains("\"delta\":\"hel\""));
    assert!(converted.contains("\"text\":\"hello\""));
    assert!(converted.contains("\"input_tokens\":3"));
    assert!(converted.contains("event: response.completed"));
    assert!(converted.contains("data: [DONE]"));
}

#[test]
fn chat_sse_converts_reasoning_inline_think_tools_and_errors_like_ccs() {
    let reasoning = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_reason","created":123,"model":"deepseek-reasoner","choices":[{"delta":{"reasoning_content":"Need context. "}}]}

data: {"id":"chatcmpl_reason","created":123,"model":"deepseek-reasoner","choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":6,"total_tokens":10,"completion_tokens_details":{"reasoning_tokens":3}}}

data: [DONE]

"#,
    );
    assert!(reasoning.contains("event: response.in_progress"));
    assert!(reasoning.contains("event: response.reasoning_summary_part.added"));
    assert!(reasoning.contains("event: response.reasoning_summary_text.delta"));
    assert!(reasoning.contains("event: response.reasoning_summary_text.done"));
    assert!(reasoning.contains("\"reasoning_content\":\"Need context. \""));
    assert!(reasoning.contains("\"type\":\"reasoning\""));
    assert!(reasoning.contains("\"text\":\"Done\""));
    assert!(reasoning.contains("\"reasoning_tokens\":3"));

    let inline_think = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_minimax","created":123,"model":"MiniMax-M2.7","choices":[{"delta":{"content":"<think>\nNeed"}}]}

data: {"id":"chatcmpl_minimax","created":123,"model":"MiniMax-M2.7","choices":[{"delta":{"content":" context.</think>\n\npong"},"finish_reason":"stop"}]}

"#,
    );
    assert!(inline_think.contains("Need context."));
    assert!(inline_think.contains("\"text\":\"pong\""));
    assert!(!inline_think.contains("<think>"));
    assert!(!inline_think.contains("</think>"));

    let tool = chat_sse_to_responses_sse(
        r#"data: {"id":"chatcmpl_tool","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather"}}]}}]}

data: {"id":"chatcmpl_tool","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":\"Tokyo\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
    );
    assert!(tool.contains("event: response.function_call_arguments.delta"));
    assert!(tool.contains("event: response.function_call_arguments.done"));
    assert!(tool.contains("\"type\":\"function_call\""));
    assert!(tool.contains("\"call_id\":\"call_1\""));

    let error = chat_sse_to_responses_sse(
        r#"event: error
data: {"error":{"message":"bad request","type":"invalid_request_error"}}

data: [DONE]

"#,
    );
    assert!(error.contains("event: response.failed"));
    assert!(error.contains("bad request"));
    assert!(error.contains("invalid_request_error"));
    assert!(!error.contains("event: response.completed"));
}

#[test]
fn chat_sse_maps_custom_tool_call_with_request_context() {
    let converted = chat_sse_to_responses_sse_with_request(
        r#"data: {"id":"chatcmpl_custom","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_custom","type":"function","function":{"name":"exec"}}]}}]}

data: {"id":"chatcmpl_custom","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"input\":"}}]}}]}

data: {"id":"chatcmpl_custom","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls -la\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
        &json!({
            "model": "gpt-5.4",
            "tools": [{ "type": "custom", "name": "exec" }]
        }),
    );

    assert!(converted.contains("response.custom_tool_call_input.delta"));
    assert_eq!(
        converted
            .matches("event: response.custom_tool_call_input.delta")
            .count(),
        1
    );
    assert!(converted.contains("\"type\":\"custom_tool_call\""));
    assert!(converted.contains("\"id\":\"ctc_call_custom\""));
    assert!(converted.contains("\"item_id\":\"ctc_call_custom\""));
    assert!(!converted.contains("\"id\":\"fc_call_custom\""));
    assert!(!converted.contains("\"item_id\":\"fc_call_custom\""));
    assert!(converted.contains("\"name\":\"exec\""));
    assert!(converted.contains("\"input\":\"ls -la\""));
    assert!(converted.contains("data: [DONE]"));
}

#[test]
fn chat_sse_waits_for_custom_tool_name_before_assigning_item_id() {
    let converted = chat_sse_to_responses_sse_with_request(
        r#"data: {"id":"chatcmpl_custom_split","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_custom_split","type":"function"}]}}]}

data: {"id":"chatcmpl_custom_split","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"exec","arguments":"{\"input\":\"pwd\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
        &json!({
            "model": "gpt-5.4",
            "tools": [{ "type": "custom", "name": "exec" }]
        }),
    );

    assert!(converted.contains("\"type\":\"custom_tool_call\""));
    assert!(converted.contains("\"id\":\"ctc_call_custom_split\""));
    assert!(converted.contains("\"item_id\":\"ctc_call_custom_split\""));
    assert!(!converted.contains("fc_call_custom_split"));
    assert!(converted.contains("\"input\":\"pwd\""));
}

#[test]
fn chat_sse_converter_handles_partial_chunks_and_utf8_boundaries() {
    let sse = "data: {\"id\":\"chatcmpl_utf8\",\"created\":123,\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\r\n\r\n";
    let bytes = sse.as_bytes();
    let split = bytes
        .windows("好".len())
        .position(|window| window == "好".as_bytes())
        .unwrap()
        + 1;

    let mut converter = ChatSseToResponsesConverter::default();
    let mut output = converter.push_bytes(&bytes[..split]);
    output.extend(converter.push_bytes(&bytes[split..]));
    output.extend(converter.finish());
    let output = String::from_utf8(output).unwrap();

    assert!(output.contains("\"delta\":\"你好\""));
    assert!(output.contains("event: response.completed"));
}

#[test]
fn chat_completions_url_normalizes_common_base_urls() {
    assert_eq!(
        chat_completions_url("https://api.example.test"),
        "https://api.example.test/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.example.test/v1"),
        "https://api.example.test/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.example.test/openai"),
        "https://api.example.test/openai/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.example.test/v1/chat/completions"),
        "https://api.example.test/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.example.test/v2"),
        "https://api.example.test/v2/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.example.test/v1beta"),
        "https://api.example.test/v1beta/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://api.example.test/openai#"),
        "https://api.example.test/openai/chat/completions"
    );
}

#[test]
fn audio_transcriptions_url_normalizes_common_base_urls() {
    assert_eq!(
        audio_transcriptions_url("https://api.example.test"),
        "https://api.example.test/v1/audio/transcriptions"
    );
    assert_eq!(
        audio_transcriptions_url("https://api.example.test/v1"),
        "https://api.example.test/v1/audio/transcriptions"
    );
    assert_eq!(
        audio_transcriptions_url("https://api.example.test/openai"),
        "https://api.example.test/openai/audio/transcriptions"
    );
    assert_eq!(
        audio_transcriptions_url("https://api.example.test/v1/audio/transcriptions"),
        "https://api.example.test/v1/audio/transcriptions"
    );
    assert_eq!(
        audio_transcriptions_url("https://api.example.test/openai#"),
        "https://api.example.test/openai/audio/transcriptions"
    );
}

#[test]
fn models_url_normalizes_common_base_urls() {
    assert_eq!(
        models_url("https://api.example.test"),
        "https://api.example.test/v1/models"
    );
    assert_eq!(
        models_url("https://api.example.test/v1"),
        "https://api.example.test/v1/models"
    );
    assert_eq!(
        models_url("https://api.example.test/v1/chat/completions"),
        "https://api.example.test/v1/models"
    );
    assert_eq!(
        models_url("https://api.example.test/models"),
        "https://api.example.test/models"
    );
    assert_eq!(
        models_url("https://api.example.test/v2"),
        "https://api.example.test/v2/models"
    );
    assert_eq!(
        models_url("https://api.example.test/v1beta"),
        "https://api.example.test/v1beta/models"
    );
    assert_eq!(
        models_url("https://api.example.test/openai#"),
        "https://api.example.test/openai/models"
    );
}

#[test]
fn models_proxy_path_matches_v1_models() {
    assert!(is_models_proxy_path("/models"));
    assert!(is_models_proxy_path("/v1/models"));
    assert!(is_models_proxy_path("/v1/models?limit=10"));
    assert!(!is_models_proxy_path("/v1/responses"));
}

#[test]
fn upstream_header_timeout_is_bounded_for_hung_providers() {
    assert!(upstream_header_timeout() >= Duration::from_secs(30));
    assert!(upstream_header_timeout() <= Duration::from_secs(60));
    assert!(upstream_stream_header_timeout() >= Duration::from_secs(120));
}

#[tokio::test]
async fn upstream_request_returns_when_provider_accepts_but_never_sends_headers() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let Ok((_stream, _addr)) = listener.accept().await else {
            return;
        };
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let started = Instant::now();
    let result = send_upstream_request_with_header_timeout(
        upstream_http_client()
            .unwrap()
            .get(format!("http://{addr}/v1/models")),
        Duration::from_millis(100),
    )
    .await;

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    server.abort();
}

#[tokio::test]
async fn aggregate_proxy_fails_over_to_next_member_in_same_request() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 11\r\ncontent-type: application/json\r\n\r\n{\"error\":1}",
    ));
    let second_server = tokio::spawn(capture_request_and_respond_once(
        second,
        "HTTP/1.1 200 OK\r\ncontent-length: 35\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp_1\",\"object\":\"response\"}",
    ));
    let mut settings = aggregate_proxy_settings(
        "failover",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let result = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5-mini","input":"hi","stream":false}"#,
        settings,
    )
    .await
    .unwrap();
    let body = result.response.bytes().await.unwrap();

    assert_eq!(result.status_code, 200);
    assert_eq!(body.as_ref(), br#"{"id":"resp_1","object":"response"}"#);
    let first_request = first_server.await.unwrap();
    let second_request = second_server.await.unwrap();
    assert!(
        !first_request
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    assert!(
        !second_request
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}

#[tokio::test]
async fn aggregate_attempts_apply_each_responses_reasoning_policy_to_an_independent_body() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 11\r\ncontent-type: application/json\r\n\r\n{\"error\":1}",
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));
    let mut settings = aggregate_proxy_settings(
        "reasoning-policy-independent",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[0].responses_reasoning_policy = ResponsesReasoningPolicy::Strip;
    settings.relay_profiles[1].responses_reasoning_policy = ResponsesReasoningPolicy::OpenAiOpaque;
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let result = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5-mini","input":[{"type":"message","role":"user","content":"before"},{"type":"reasoning","content":"visible","encrypted_content":"cipher"},{"type":"function_call","call_id":"call-1","name":"lookup"}],"stream":false}"#,
        settings,
    )
    .await
    .unwrap();
    assert_eq!(result.status_code, 200);

    let first_request = first_server.await.unwrap();
    let first_body = first_request
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str::<serde_json::Value>(body).ok())
        .unwrap();
    assert!(
        first_body["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item.get("type").and_then(serde_json::Value::as_str) != Some("reasoning"))
    );

    let (_, second_body) = second_server.await.unwrap();
    assert_eq!(second_body["input"][1]["type"], "reasoning");
    assert_eq!(second_body["input"][1]["content"], json!([]));
    assert_eq!(second_body["input"][2]["type"], "function_call");
}

#[tokio::test]
async fn priority_fallback_waits_for_third_failure_before_failing_over() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = first.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 11\r\ncontent-type: application/json\r\n\r\n{\"error\":1}",
                )
                .await
                .unwrap();
        }
    });
    let second_server = tokio::spawn(capture_request_and_respond_once(
        second,
        "HTTP/1.1 200 OK\r\ncontent-length: 35\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp_1\",\"object\":\"response\"}",
    ));
    let mut settings = aggregate_proxy_settings(
        "priority-threshold",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.aggregate_relay_profiles[0].strategy = AggregateRelayStrategy::PriorityFallback;

    let unattended_turn_request =
        r#"{"model":"gpt-5-mini","input":"hi","stream":false,"conversation":"unattended-turn-1"}"#;
    for expected_failures in 1..=2 {
        let result =
            open_responses_proxy_request_with_settings(unattended_turn_request, settings.clone())
                .await
                .unwrap();
        assert_eq!(result.status_code, 500);
        let _ = result.response.bytes().await.unwrap();
        let status = priority_fallback_cooldown_status(&settings);
        assert_eq!(status.members[0].consecutive_failures, expected_failures);
        assert_eq!(status.members[0].cooldown_remaining_seconds, 0);
    }

    let result =
        open_responses_proxy_request_with_settings(unattended_turn_request, settings.clone())
            .await
            .unwrap();
    assert_eq!(result.status_code, 200);
    let _ = result.response.bytes().await.unwrap();
    let status = priority_fallback_cooldown_status(&settings);
    assert_eq!(status.members[0].consecutive_failures, 0);
    assert!(status.members[0].cooldown_remaining_seconds > 0);
    assert_eq!(status.members[1].consecutive_failures, 0);

    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn priority_fallback_hides_429_by_failing_over_in_the_same_request() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}",
    ));
    let second_server = tokio::spawn(capture_request_and_respond_once(
        second,
        "HTTP/1.1 200 OK\r\ncontent-length: 35\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp_1\",\"object\":\"response\"}",
    ));
    let mut settings = aggregate_proxy_settings(
        "priority-429-silent-failover",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.aggregate_relay_profiles[0].strategy = AggregateRelayStrategy::PriorityFallback;

    let result = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5-mini","input":"hi","stream":false}"#,
        settings.clone(),
    )
    .await
    .unwrap();
    let body = result.response.bytes().await.unwrap();

    assert_eq!(result.status_code, 200);
    assert_eq!(body.as_ref(), br#"{"id":"resp_1","object":"response"}"#);
    let status = priority_fallback_cooldown_status(&settings);
    assert_eq!(status.members[0].consecutive_failures, 1);
    assert_eq!(status.members[0].cooldown_remaining_seconds, 0);
    assert_eq!(status.members[1].consecutive_failures, 0);

    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn aggregate_proxy_preserves_final_upstream_status_code() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}",
    ));
    let second_server = tokio::spawn(capture_request_and_respond_once(
        second,
        "HTTP/1.1 418 I'm a teapot\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":418}",
    ));
    let mut settings = aggregate_proxy_settings(
        "status-preservation",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let result = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5-mini","input":"hi","stream":false}"#,
        settings,
    )
    .await
    .unwrap();

    assert_eq!(result.status_code, 418);
    assert_eq!(result.response.status().as_u16(), 418);
    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn aggregate_code_mode_host_forwards_only_function_tools() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": "run a terminal command",
        "stream": false,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [
            { "type": "function", "name": "exec_command", "parameters": { "type": "object" } },
            { "type": "function", "name": "write_stdin", "parameters": { "type": "object" } },
            { "type": "custom", "name": "apply_patch" },
            { "type": "tool_search" },
            { "type": "web_search" }
        ]
    });
    let mut settings = aggregate_proxy_settings(
        "code-mode-host-on",
        format!("http://{target_addr}/v1"),
        "http://127.0.0.1:9/v1".to_string(),
    );
    settings.aggregate_relay_profiles[0].code_mode_host = true;

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();

    assert_eq!(
        upstream_body["tools"],
        json!([
            { "type": "function", "name": "exec_command", "parameters": { "type": "object" } },
            { "type": "function", "name": "write_stdin", "parameters": { "type": "object" } }
        ])
    );
    assert_eq!(upstream_body["tool_choice"], "auto");
    assert_eq!(upstream_body["parallel_tool_calls"], true);
}

#[tokio::test]
async fn aggregate_code_mode_host_is_opt_in() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": "keep all tools",
        "stream": false,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [
            { "type": "function", "name": "exec_command", "parameters": { "type": "object" } },
            { "type": "custom", "name": "apply_patch" },
            { "type": "tool_search" },
            { "type": "web_search" }
        ]
    });
    let settings = aggregate_proxy_settings(
        "code-mode-host-off",
        format!("http://{target_addr}/v1"),
        "http://127.0.0.1:9/v1".to_string(),
    );

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();

    assert_eq!(upstream_body, request);
}

#[tokio::test]
async fn aggregate_code_mode_host_removes_empty_tool_controls() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": "no supported tools",
        "stream": false,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [
            { "type": "custom", "name": "apply_patch" },
            { "type": "tool_search" },
            { "type": "web_search" }
        ]
    });
    let mut settings = aggregate_proxy_settings(
        "code-mode-host-empty",
        format!("http://{target_addr}/v1"),
        "http://127.0.0.1:9/v1".to_string(),
    );
    settings.aggregate_relay_profiles[0].code_mode_host = true;

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();

    assert!(upstream_body.get("tools").is_none());
    assert!(upstream_body.get("tool_choice").is_none());
    assert!(upstream_body.get("parallel_tool_calls").is_none());
}

#[tokio::test]
async fn model_route_uses_target_responses_provider_without_mutating_request() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "instructions": "Use the available tools when needed.",
        "input": [{ "role": "user", "content": "inspect the workspace" }],
        "stream": false,
        "reasoning": { "effort": "high", "summary": "auto" },
        "service_tier": "priority",
        "truncation": "disabled",
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [{
            "type": "function",
            "name": "read_file",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        }],
        "metadata": { "route_test": true }
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (headers, upstream_body) = target_server.await.unwrap();

    assert!(headers.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-target")
    );
    assert_eq!(upstream_body, request);
}

#[tokio::test]
async fn model_route_supports_no_auth_target_without_authorization_header() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": "hello",
        "stream": false
    });
    let mut settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));
    settings.relay_profiles[1].relay_mode = RelayMode::PureApi;
    settings.relay_profiles[1].no_auth = true;
    settings.relay_profiles[1].api_key.clear();

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    let (headers, upstream_body) = target_server.await.unwrap();

    assert_eq!(result.status_code, 200);
    assert!(!headers.to_ascii_lowercase().contains("authorization:"));
    assert_eq!(upstream_body, request);
}

#[tokio::test]
async fn model_route_can_rewrite_only_the_target_model_name() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": "hello",
        "stream": false,
        "tools": [{ "type": "function", "name": "lookup", "parameters": { "type": "object" } }],
        "truncation": "disabled"
    });
    let settings = model_route_settings(
        "gpt-5.6-luna",
        "provider-luna-v2",
        format!("http://{target_addr}/v1"),
    );

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();

    let mut expected = request;
    expected["model"] = json!("provider-luna-v2");
    assert_eq!(upstream_body, expected);
}

#[tokio::test]
async fn responses_proxy_normalizes_legacy_custom_tool_item_ids_only() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [
            {
                "type": "custom_tool_call",
                "id": "fc_legacy_custom_item",
                "call_id": "call_legacy_custom",
                "name": "exec",
                "input": "pwd"
            },
            {
                "type": "message",
                "role": "user",
                "content": "continue"
            }
        ],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();

    assert_eq!(upstream_body["input"][0]["id"], "ctc_legacy_custom_item");
    assert_eq!(upstream_body["input"][0]["call_id"], "call_legacy_custom");
    assert_eq!(upstream_body["input"][1]["type"], "message");
}

#[tokio::test]
async fn responses_proxy_normalizes_relay_ids_for_compatible_upstream() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let foreign_id = "chatcmpl-202609200918258761407968268d9d6WGd5KQKm_msg_0";
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [
            {
                "type": "message",
                "id": foreign_id,
                "role": "user",
                "content": "continue"
            },
            {
                "type": "function_call",
                "id": "chatcmpl-202609200918258761407968268d9d6WGd5KQKm_fc_1",
                "call_id": "call_relay",
                "name": "lookup"
            },
            {
                "type": "function_call_output",
                "id": "chatcmpl-202609200918258761407968268d9d6WGd5KQKm_fco_1",
                "call_id": "call_relay",
                "output": "done"
            }
        ],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();

    assert_eq!(upstream_body["input"][0]["id"], format!("msg_{foreign_id}"));
    assert_eq!(
        upstream_body["input"][1]["id"],
        "fc_chatcmpl-202609200918258761407968268d9d6WGd5KQKm_fc_1"
    );
    assert_eq!(
        upstream_body["input"][2]["id"],
        "fco_chatcmpl-202609200918258761407968268d9d6WGd5KQKm_fco_1"
    );
}

#[tokio::test]
async fn responses_proxy_merges_duplicate_custom_tool_outputs() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [
            {
                "type": "custom_tool_call_output",
                "id": "ctco_first",
                "call_id": "call_duplicate",
                "output": "Script completed"
            },
            {
                "type": "custom_tool_call_output",
                "id": "ctco_second",
                "call_id": "call_duplicate",
                "name": "exec",
                "output": "5610"
            }
        ],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, upstream_body) = target_server.await.unwrap();
    let items = upstream_body["input"].as_array().unwrap();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["type"], "custom_tool_call_output");
    assert_eq!(items[0]["id"], "ctco_first");
    assert_eq!(items[0]["call_id"], "call_duplicate");
    assert_eq!(items[0]["name"], "exec");
    assert_eq!(
        items[0]["output"],
        json!([
            {"type": "input_text", "text": "Script completed"},
            {"type": "input_text", "text": "5610"}
        ])
    );
}

async fn read_json_http_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request closed before headers completed");
        buffer.extend_from_slice(&chunk[..read]);
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            })
            .unwrap_or(0);
        break (header_end + 4, content_length);
    };
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request closed before body completed");
        buffer.extend_from_slice(&chunk[..read]);
    }
    serde_json::from_slice(&buffer[header_end..header_end + content_length]).unwrap()
}

/// 依次回放给定响应；序列用尽后再等一小段时间，把多余的请求也记录下来，
/// 让"不应该重试"的断言能明确失败而不是只在连接层面报错。
async fn capture_json_requests_respond_sequence(
    listener: tokio::net::TcpListener,
    responses: Vec<String>,
) -> Vec<serde_json::Value> {
    let mut captured = Vec::new();
    for response in responses {
        let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_secs(10), listener.accept()).await
        else {
            break;
        };
        captured.push(read_json_http_request(&mut stream).await);
        stream.write_all(response.as_bytes()).await.unwrap();
        let _ = stream.shutdown().await;
    }
    if let Ok(Ok((mut stream, _))) =
        tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
    {
        captured.push(read_json_http_request(&mut stream).await);
        let _ = stream
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{}",
            )
            .await;
    }
    captured
}

fn error_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
        body.len(),
        body
    )
}

fn ok_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
        body.len(),
        body
    )
}

#[tokio::test]
async fn responses_proxy_retries_without_encrypted_reasoning_when_upstream_rejects_blob() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let encrypted_error = r#"{"error":{"message":"The encrypted content for item rs_blob could not be verified. Reason: Encrypted content could not be decrypted or parsed.","type":"invalid_request_error","code":"invalid_encrypted_content"}}"#;
    let retry_success = r#"{"id":"resp_retry_ok","object":"response"}"#;
    let server = tokio::spawn(capture_json_requests_respond_sequence(
        target,
        vec![
            error_response("400 Bad Request", encrypted_error),
            ok_response(retry_success),
        ],
    ));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [
            {"type": "message", "id": "msg_keep", "role": "user", "content": "continue"},
            {
                "type": "reasoning",
                "id": "rs_blob",
                "summary": [{"type": "summary_text", "text": "previous thinking"}],
                "encrypted_content": "opaque-blob"
            },
            {"type": "function_call", "id": "fc_keep", "call_id": "call_keep", "name": "lookup", "arguments": "{}"},
            {"type": "function_call_output", "id": "fco_keep", "call_id": "call_keep", "output": "done"}
        ],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    let body = result.response.bytes().await.unwrap();

    assert_eq!(result.status_code, 200);
    assert_eq!(body.as_ref(), retry_success.as_bytes());
    let captured = server.await.unwrap();
    assert_eq!(
        captured.len(),
        2,
        "命中 encrypted content 校验失败后必须重试一次"
    );
    let first_items = captured[0]["input"].as_array().unwrap();
    let second_items = captured[1]["input"].as_array().unwrap();
    assert!(
        first_items
            .iter()
            .any(|item| item["type"] == "reasoning"
                && item["encrypted_content"] == "opaque-blob"),
        "首个请求必须保留原始 encrypted reasoning"
    );
    assert!(
        !second_items.iter().any(|item| item["type"] == "reasoning"),
        "重试请求必须去掉无法解密的 reasoning 项"
    );
    for item_type in ["message", "function_call", "function_call_output"] {
        let before = first_items
            .iter()
            .find(|item| item["type"] == item_type)
            .unwrap();
        let after = second_items
            .iter()
            .find(|item| item["type"] == item_type)
            .unwrap();
        assert_eq!(before, after, "{item_type} 之外的改写不允许发生");
    }
}

#[tokio::test]
async fn responses_proxy_keeps_original_error_when_retry_predicate_does_not_match() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let upstream_error = r#"{"error":{"message":"unknown tool: lookup","type":"invalid_request_error"}}"#;
    let server = tokio::spawn(capture_json_requests_respond_sequence(
        target,
        vec![error_response("400 Bad Request", upstream_error)],
    ));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [
            {"type": "message", "id": "msg_keep", "role": "user", "content": "continue"},
            {
                "type": "reasoning",
                "id": "rs_blob",
                "summary": [{"type": "summary_text", "text": "previous thinking"}],
                "encrypted_content": "opaque-blob"
            }
        ],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    let body = result.response.bytes().await.unwrap();

    assert_eq!(result.status_code, 400);
    assert_eq!(body.as_ref(), upstream_error.as_bytes());
    assert_eq!(result.content_type, "application/json");
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 1, "非 encrypted-content 错误不允许重试");
}

#[tokio::test]
async fn responses_proxy_does_not_retry_without_encrypted_reasoning_in_request() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let upstream_error =
        r#"{"error":{"message":"encrypted content could not be parsed","code":"invalid_encrypted_content"}}"#;
    let server = tokio::spawn(capture_json_requests_respond_sequence(
        target,
        vec![error_response("400 Bad Request", upstream_error)],
    ));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [{"type": "message", "id": "msg_keep", "role": "user", "content": "continue"}],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    let body = result.response.bytes().await.unwrap();

    assert_eq!(result.status_code, 400);
    assert_eq!(body.as_ref(), upstream_error.as_bytes());
    let captured = server.await.unwrap();
    assert_eq!(
        captured.len(),
        1,
        "请求里没有 encrypted reasoning 时不得触发重试"
    );
}

#[test]
fn namespace_restore_recovers_prefixed_collaboration_tool_names_without_map() {
    // 模型把多代理工具名再套一层前缀（历史里出现过的两种写法都要能还原），
    // 而这一轮请求根本没有建立 namespace 映射表。
    let body = json!({
        "id": "resp_1",
        "output": [
            {
                "type": "function_call",
                "id": "fc_collab",
                "call_id": "call_collab",
                "name": "codexpp_native_collaboration__spawn_agent",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "id": "fc_collab_plain",
                "call_id": "call_collab_plain",
                "name": "collaboration__wait_agent",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "id": "fc_other",
                "call_id": "call_other",
                "name": "functions__exec",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "id": "fc_foreign_ns",
                "call_id": "call_foreign_ns",
                "name": "other_namespace__spawn_agent",
                "arguments": "{}"
            }
        ]
    });
    let namespaces = std::collections::BTreeMap::new();

    let restored: serde_json::Value = serde_json::from_slice(
        &restore_responses_tool_namespace_json(&serde_json::to_vec(&body).unwrap(), &namespaces),
    )
    .unwrap();

    assert_eq!(restored["output"][0]["namespace"], "collaboration");
    assert_eq!(restored["output"][0]["name"], "spawn_agent");
    assert_eq!(restored["output"][1]["namespace"], "collaboration");
    assert_eq!(restored["output"][1]["name"], "wait_agent");
    // 其它命名空间的拍扁名和非多代理工具不得被改写。
    assert_eq!(restored["output"][2]["name"], "functions__exec");
    assert!(restored["output"][2].get("namespace").is_none());
    assert_eq!(
        restored["output"][3]["name"],
        "other_namespace__spawn_agent"
    );
    assert!(restored["output"][3].get("namespace").is_none());
}

#[test]
fn namespace_restore_keeps_exact_map_hits_authoritative() {
    let body = json!({
        "output": [
            {
                "type": "function_call",
                "id": "fc_collab",
                "call_id": "call_collab",
                "name": "codexpp_native_collaboration__spawn_agent",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "id": "fc_exact",
                "call_id": "call_exact",
                "name": "collaboration__spawn_agent",
                "arguments": "{}"
            }
        ]
    });
    let mut namespaces = std::collections::BTreeMap::new();
    namespaces.insert(
        "collaboration__spawn_agent".to_string(),
        ("collaboration".to_string(), "spawn_agent".to_string()),
    );

    let restored: serde_json::Value = serde_json::from_slice(
        &restore_responses_tool_namespace_json(&serde_json::to_vec(&body).unwrap(), &namespaces),
    )
    .unwrap();

    assert_eq!(restored["output"][0]["namespace"], "collaboration");
    assert_eq!(restored["output"][0]["name"], "spawn_agent");
    assert_eq!(restored["output"][1]["namespace"], "collaboration");
    assert_eq!(restored["output"][1]["name"], "spawn_agent");
}

#[test]
fn namespace_sse_rewriter_restores_prefixed_collaboration_call_without_map() {
    let mut rewriter = ResponsesNamespaceSseRewriter::new(std::collections::BTreeMap::new());
    let frame = concat!(
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",",
        "\"id\":\"fc_1\",\"call_id\":\"call_1\",",
        "\"name\":\"codexpp_native_collaboration__followup_task\",\"arguments\":\"{}\"}}\n\n"
    );

    let mut output = rewriter.push_bytes(frame.as_bytes());
    output.extend(rewriter.finish());
    let text = String::from_utf8(output).unwrap();

    assert!(
        text.contains("\"namespace\":\"collaboration\""),
        "SSE 帧里的拍扁名必须还原: {text}"
    );
    assert!(text.contains("\"name\":\"followup_task\""));
    assert!(!text.contains("codexpp_native_collaboration__followup_task"));
}

#[tokio::test]
async fn upstream_request_sends_opencode_session_from_prompt_cache_key() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "prompt_cache_key": "cache-abc-123",
        "input": [{"type": "message", "role": "user", "content": "hi"}],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (headers, _) = target_server.await.unwrap();
    let lower = headers.to_ascii_lowercase();
    assert!(
        lower.contains("x-opencode-session: cache-abc-123"),
        "OpenCode Go 需要 x-opencode-session，实到请求头: {headers}"
    );
}

#[tokio::test]
async fn request_session_id_header_wins_over_prompt_cache_key() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "prompt_cache_key": "cache-abc-123",
        "input": [{"type": "message", "role": "user", "content": "hi"}],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = with_request_session_id(
        Some("header-session-9".to_string()),
        open_responses_proxy_request_with_settings(&request.to_string(), settings),
    )
    .await
    .unwrap();
    assert_eq!(result.status_code, 200);
    let (headers, _) = target_server.await.unwrap();
    let lower = headers.to_ascii_lowercase();
    assert!(lower.contains("x-opencode-session: header-session-9"));
    assert!(!lower.contains("x-opencode-session: cache-abc-123"));
}

#[tokio::test]
async fn model_route_preserves_responses_compact_endpoint() {
    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let request = json!({
        "model": "gpt-5.6-luna",
        "input": [{ "role": "user", "content": "compact this conversation" }],
        "stream": false
    });
    let settings = model_route_settings("gpt-5.6-luna", "", format!("http://{target_addr}/v1"));

    let result = open_responses_proxy_request_with_settings_for_path(
        &request.to_string(),
        settings,
        "/v1/responses/compact",
    )
    .await
    .unwrap();
    assert_eq!(result.status_code, 200);
    let (headers, upstream_body) = target_server.await.unwrap();

    assert!(headers.starts_with("POST /v1/responses/compact HTTP/1.1"));
    assert_eq!(upstream_body, request);
}

#[tokio::test]
async fn model_route_uses_exact_match_and_keeps_other_models_on_source_provider() {
    let source = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let source_addr = source.local_addr().unwrap();
    let source_server = tokio::spawn(capture_json_request_once(source));
    let request = json!({
        "model": "gpt-5.6-luna-preview",
        "input": "hello",
        "stream": false,
        "tools": [{ "type": "function", "name": "lookup", "parameters": { "type": "object" } }]
    });
    let mut settings =
        model_route_settings("gpt-5.6-luna", "", "http://127.0.0.1:9/v1".to_string());
    settings.relay_profiles[0].base_url = format!("http://{source_addr}/v1");

    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (headers, upstream_body) = source_server.await.unwrap();

    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-source")
    );
    assert_eq!(upstream_body, request);
}

#[tokio::test]
async fn disabled_model_route_uses_source_and_expired_route_uses_target() {
    let source = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let source_addr = source.local_addr().unwrap();
    let source_server = tokio::spawn(capture_json_request_once(source));
    let request = json!({ "model": "gpt-5.6-terra", "input": "source", "stream": false });
    let mut settings = model_route_settings(
        "gpt-5.6-terra",
        "glm-5.3",
        "http://127.0.0.1:9/v1".to_string(),
    );
    settings.relay_profiles[0].base_url = format!("http://{source_addr}/v1");
    settings.relay_profiles[0].model_routes[0].enabled = false;
    settings.relay_profiles[0].model_routes[0].restore_at = None;

    open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    let (headers, body) = source_server.await.unwrap();
    assert!(headers.to_ascii_lowercase().contains("bearer sk-source"));
    assert_eq!(body["model"], "gpt-5.6-terra");

    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_server = tokio::spawn(capture_json_request_once(target));
    let mut expired = model_route_settings(
        "gpt-5.6-terra",
        "glm-5.3",
        format!("http://{target_addr}/v1"),
    );
    expired.relay_profiles[0].model_routes[0].enabled = false;
    expired.relay_profiles[0].model_routes[0].restore_at = Some(1);

    open_responses_proxy_request_with_settings(&request.to_string(), expired)
        .await
        .unwrap();
    let (headers, body) = target_server.await.unwrap();
    assert!(headers.to_ascii_lowercase().contains("bearer sk-target"));
    assert_eq!(body["model"], "glm-5.3");
}

#[tokio::test]
async fn model_route_rejects_missing_or_non_responses_targets() {
    let mut missing = model_route_settings("gpt-5.6-luna", "", "http://127.0.0.1:9/v1".to_string());
    missing.relay_profiles.pop();
    let error = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5.6-luna","input":"hi"}"#,
        missing,
    )
    .await
    .err()
    .expect("missing target should fail");
    assert!(error.to_string().contains("模型路由目标供应商不存在"));

    let mut chat = model_route_settings("gpt-5.6-luna", "", "http://127.0.0.1:9/v1".to_string());
    chat.relay_profiles[1].protocol = RelayProtocol::ChatCompletions;
    let error = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5.6-luna","input":"hi"}"#,
        chat,
    )
    .await
    .err()
    .expect("chat target should fail");
    assert!(error.to_string().contains("必须使用 Responses API"));
}

#[tokio::test]
async fn aggregate_stream_request_sends_sse_accept_header() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let fallback = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let fallback_addr = fallback.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 14\r\ncontent-type: text/event-stream\r\n\r\ndata: [DONE]\n\n",
            )
            .await
            .unwrap();
        request
    });
    let fallback_server = tokio::spawn(respond_once(
        fallback,
        "HTTP/1.1 200 OK\r\ncontent-length: 14\r\ncontent-type: text/event-stream\r\n\r\ndata: [DONE]\n\n",
    ));
    let settings = aggregate_proxy_settings(
        "stream",
        format!("http://{addr}/v1"),
        format!("http://{fallback_addr}/v1"),
    );

    let result = open_responses_proxy_request_with_settings(
        r#"{"model":"gpt-5-mini","input":"hi","stream":true}"#,
        settings,
    )
    .await
    .unwrap();
    let request = server.await.unwrap();

    assert_eq!(result.status_code, 200);
    assert!(result.is_stream);
    assert!(
        request
            .to_ascii_lowercase()
            .contains("accept: text/event-stream")
    );
    fallback_server.abort();
}

async fn respond_once(listener: tokio::net::TcpListener, response: &'static str) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = [0; 1024];
    let _ = stream.read(&mut buffer).await.unwrap();
    stream.write_all(response.as_bytes()).await.unwrap();
}

async fn capture_request_and_respond_once(
    listener: tokio::net::TcpListener,
    response: &'static str,
) -> String {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = [0; 4096];
    let read = stream.read(&mut buffer).await.unwrap();
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    stream.write_all(response.as_bytes()).await.unwrap();
    request
}

async fn capture_json_request_once(
    listener: tokio::net::TcpListener,
) -> (String, serde_json::Value) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request closed before headers completed");
        buffer.extend_from_slice(&chunk[..read]);
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            })
            .unwrap_or(0);
        break (header_end + 4, content_length);
    };
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request closed before body completed");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let headers = String::from_utf8_lossy(&buffer[..header_end - 4]).to_string();
    let body = serde_json::from_slice(&buffer[header_end..header_end + content_length]).unwrap();
    let response_body = r#"{"id":"resp_model_route","object":"response"}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    (headers, body)
}

fn model_route_settings(
    source_model: &str,
    target_model: &str,
    target_base_url: String,
) -> BackendSettings {
    BackendSettings {
        active_relay_id: "source".to_string(),
        relay_profiles: vec![
            RelayProfile {
                id: "source".to_string(),
                name: "source".to_string(),
                base_url: "http://127.0.0.1:9/v1".to_string(),
                api_key: "sk-source".to_string(),
                model_routes: vec![RelayModelRoute {
                    model: source_model.to_string(),
                    target_relay_id: "target".to_string(),
                    target_model: target_model.to_string(),
                    enabled: true,
                    restore_at: None,
                }],
                ..RelayProfile::default()
            },
            RelayProfile {
                id: "target".to_string(),
                name: "target".to_string(),
                base_url: target_base_url,
                api_key: "sk-target".to_string(),
                protocol: RelayProtocol::Responses,
                ..RelayProfile::default()
            },
        ],
        ..BackendSettings::default()
    }
}

fn aggregate_proxy_settings(
    id_suffix: &str,
    first_base_url: String,
    second_base_url: String,
) -> BackendSettings {
    let first_id = format!("proxy-{id_suffix}-a");
    let second_id = format!("proxy-{id_suffix}-b");
    let aggregate_id = format!("proxy-{id_suffix}-agg");
    BackendSettings {
        relay_profiles: vec![
            RelayProfile {
                id: first_id.clone(),
                name: "first".to_string(),
                base_url: first_base_url,
                api_key: "sk-first".to_string(),
                ..RelayProfile::default()
            },
            RelayProfile {
                id: second_id.clone(),
                name: "second".to_string(),
                base_url: second_base_url,
                api_key: "sk-second".to_string(),
                ..RelayProfile::default()
            },
            RelayProfile {
                id: aggregate_id.clone(),
                name: "aggregate".to_string(),
                relay_mode: RelayMode::Aggregate,
                ..RelayProfile::default()
            },
        ],
        active_relay_id: aggregate_id.clone(),
        active_aggregate_relay_id: aggregate_id.clone(),
        aggregate_relay_profiles: vec![AggregateRelayProfile {
            id: aggregate_id,
            name: "aggregate".to_string(),
            session_provider: RelaySessionProvider::Custom,
            code_mode_host: false,
            strategy: AggregateRelayStrategy::RequestRoundRobin,
            members: vec![
                AggregateRelayMember {
                    relay_id: first_id,
                    weight: 1,
                },
                AggregateRelayMember {
                    relay_id: second_id,
                    weight: 1,
                },
            ],
        }],
        ..BackendSettings::default()
    }
}
#[tokio::test]
async fn audio_transcriptions_proxy_forwards_multipart_body() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            let request = String::from_utf8_lossy(&buffer);
            let Some((headers, body)) = request.split_once("\r\n\r\n") else {
                continue;
            };
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                })
                .unwrap_or(0);
            if body.as_bytes().len() >= content_length {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buffer).to_string();
        let body = r#"{"text":"ok"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        request
    });
    write_chat_relay_settings(temp.path(), &format!("http://{addr}/v1"), "");
    let boundary = "codex-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-4o-mini-transcribe\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nabc\r\n--{boundary}--\r\n"
    );

    let upstream = open_audio_transcriptions_proxy_request(
        body.as_bytes(),
        &format!("multipart/form-data; boundary={boundary}"),
        Some("Original-Codex-UA/1.0"),
    )
    .await
    .unwrap();
    assert_eq!(upstream.status_code, 200);
    let request = server.await.unwrap();
    assert!(request.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
    assert!(
        request.contains("content-type: multipart/form-data; boundary=codex-boundary")
            || request.contains("Content-Type: multipart/form-data; boundary=codex-boundary")
    );
    assert!(request.contains("gpt-4o-mini-transcribe"));
    assert!(request.contains("abc"));
}

#[tokio::test]
async fn chat_completions_proxy_uses_configured_user_agent() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));
    let server = spawn_chat_server();
    write_chat_relay_settings(temp.path(), &server.base_url, "Configured-Codex-UA/1.0");

    let upstream = open_chat_completions_proxy_request(
        r#"{"model":"gpt-5.5","messages":[{"role":"user","content":"hello"}]}"#,
        Some("Original-Codex-UA/1.0"),
    )
    .await
    .unwrap();
    assert_eq!(upstream.status_code, 200);

    let request = server.finish();
    assert_eq!(request.user_agent, "Configured-Codex-UA/1.0");
}

#[tokio::test]
async fn chat_completions_proxy_passes_through_original_user_agent_when_unconfigured() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));
    let server = spawn_chat_server();
    write_chat_relay_settings(temp.path(), &server.base_url, "");

    let upstream = open_chat_completions_proxy_request(
        r#"{"model":"gpt-5.5","messages":[{"role":"user","content":"hello"}]}"#,
        Some("Original-Codex-UA/1.0"),
    )
    .await
    .unwrap();
    assert_eq!(upstream.status_code, 200);

    let request = server.finish();
    assert_eq!(request.user_agent, "Original-Codex-UA/1.0");
}

#[tokio::test]
async fn responses_proxy_passes_through_original_user_agent_when_unconfigured() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));
    let server = spawn_chat_server();
    write_chat_relay_settings(temp.path(), &server.base_url, "");

    let upstream = open_responses_proxy_request(
        r#"{"model":"gpt-5.5","input":"hello","stream":false}"#,
        Some("Original-Codex-UA/1.0"),
    )
    .await
    .unwrap();
    assert_eq!(upstream.status_code, 200);

    let request = server.finish();
    assert_eq!(request.user_agent, "Original-Codex-UA/1.0");
}

#[tokio::test]
async fn models_proxy_passes_through_original_user_agent_when_unconfigured() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));
    let server = spawn_chat_server();
    write_chat_relay_settings(temp.path(), &server.base_url, "");

    let upstream = open_models_proxy_request(Some("Original-Codex-UA/1.0"))
        .await
        .unwrap();
    assert_eq!(upstream.status_code, 200);

    let request = server.finish();
    assert_eq!(request.user_agent, "Original-Codex-UA/1.0");
}

#[tokio::test]
async fn no_auth_proxy_endpoints_omit_authorization_header() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let server = spawn_chat_server();
    write_no_auth_relay_settings(temp.path(), &server.base_url, "responses");
    let upstream = open_responses_proxy_request(
        r#"{"model":"gpt-5.5","input":"hello","stream":false}"#,
        None,
    )
    .await
    .unwrap();
    assert_eq!(upstream.status_code, 200);
    assert_eq!(server.finish().authorization, None);

    let server = spawn_chat_server();
    write_no_auth_relay_settings(temp.path(), &server.base_url, "chatCompletions");
    let upstream = open_chat_completions_proxy_request(
        r#"{"model":"gpt-5.5","messages":[{"role":"user","content":"hello"}]}"#,
        None,
    )
    .await
    .unwrap();
    assert_eq!(upstream.status_code, 200);
    assert_eq!(server.finish().authorization, None);

    let server = spawn_chat_server();
    write_no_auth_relay_settings(temp.path(), &server.base_url, "responses");
    let upstream = open_models_proxy_request(None).await.unwrap();
    assert_eq!(upstream.status_code, 200);
    assert_eq!(server.finish().authorization, None);

    let server = spawn_chat_server();
    write_no_auth_relay_settings(temp.path(), &server.base_url, "responses");
    let upstream = open_audio_transcriptions_proxy_request(b"audio", "audio/wav", None)
        .await
        .unwrap();
    assert_eq!(upstream.status_code, 200);
    assert_eq!(server.finish().authorization, None);
}

#[tokio::test]
async fn no_auth_profile_test_omits_authorization_header() {
    let server = spawn_chat_server();
    let profile = RelayProfile {
        base_url: server.base_url.clone(),
        upstream_base_url: server.base_url.clone(),
        relay_mode: RelayMode::PureApi,
        no_auth: true,
        api_key: String::new(),
        ..RelayProfile::default()
    };

    let result = test_relay_profile(&profile, "gpt-5.5").await.unwrap();

    assert_eq!(result.http_status, 200);
    assert_eq!(server.finish().authorization, None);
}

fn write_chat_relay_settings(settings_dir: &Path, base_url: &str, user_agent: &str) {
    let settings = json!({
        "relayProfiles": [{
            "id": "chat",
            "name": "Chat",
            "baseUrl": base_url,
            "upstreamBaseUrl": base_url,
            "apiKey": "sk-test",
            "protocol": "chatCompletions",
            "relayMode": "mixedApi",
            "userAgent": user_agent
        }],
        "activeRelayId": "chat"
    });
    std::fs::write(
        settings_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
}

fn write_no_auth_relay_settings(settings_dir: &Path, base_url: &str, protocol: &str) {
    let settings = json!({
        "relayProfiles": [{
            "id": "no-auth",
            "name": "No Auth",
            "baseUrl": base_url,
            "upstreamBaseUrl": base_url,
            "apiKey": "",
            "protocol": protocol,
            "relayMode": "pureApi",
            "noAuth": true
        }],
        "activeRelayId": "no-auth"
    });
    std::fs::write(
        settings_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
}

struct SettingsPathGuard {
    previous: Option<PathBuf>,
}

fn settings_path_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl SettingsPathGuard {
    fn set(path: PathBuf) -> Self {
        let previous = codex_plus_core::paths::set_settings_path_for_tests(Some(path));
        Self { previous }
    }
}

impl Drop for SettingsPathGuard {
    fn drop(&mut self) {
        codex_plus_core::paths::set_settings_path_for_tests(self.previous.take());
    }
}

struct ChatServer {
    base_url: String,
    handle: thread::JoinHandle<ChatRequest>,
}

impl ChatServer {
    fn finish(self) -> ChatRequest {
        self.handle.join().unwrap()
    }
}

struct ChatRequest {
    user_agent: String,
    authorization: Option<String>,
}

fn spawn_chat_server() -> ChatServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let base_url = format!("http://{address}/v1");
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < std::time::Duration::from_secs(5),
                        "test upstream did not receive a request"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("failed to accept test request: {error}"),
            }
        };
        let mut buffer = [0u8; 4096];
        let bytes = loop {
            match stream.read(&mut buffer) {
                Ok(0) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Ok(bytes) => break bytes,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("failed to read test request: {error}"),
            }
        };
        let request = String::from_utf8_lossy(&buffer[..bytes]).to_string();
        let user_agent = request
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("user-agent")
                        .then(|| value.trim().to_string())
                })
            })
            .unwrap_or_default();
        let authorization = request.lines().find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_string())
            })
        });
        let body = r#"{"id":"chatcmpl-test","object":"chat.completion","choices":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        ChatRequest {
            user_agent,
            authorization,
        }
    });
    ChatServer { base_url, handle }
}

// ── tool 输出中的图片（issue #1996）────────────────────────────────────
//
// `view_image` 的结果以 `function_call_output.output[].input_image` 回来。这个数组
// 曾被整体 JSON 序列化成 tool 消息的字符串 content，于是 base64 被当普通文本送进上游
// tokenizer —— 一张 2MB 的 PNG 膨胀到约 200 万 token 并撑爆上下文窗口。

const TEST_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB";

/// 收集 JSON 里所有**不在** `image_url` 子树下的字符串，即会被上游当文本 tokenize 的部分。
fn tokenizable_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "image_url" {
                    continue;
                }
                tokenizable_strings(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                tokenizable_strings(item, out);
            }
        }
        serde_json::Value::String(text) => out.push(text.clone()),
        _ => {}
    }
}

fn assert_no_base64_in_text(converted: &serde_json::Value) {
    let mut strings = Vec::new();
    tokenizable_strings(converted, &mut strings);
    for text in strings {
        assert!(
            !text.contains("data:image/"),
            "base64 图片泄漏进文本字段: {text}"
        );
    }
}

fn image_tool_call_input(output: serde_json::Value) -> serde_json::Value {
    json!({
        "model": "deepseek-v4-flash-vision-exp",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "look at this" }]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "view_image",
                "arguments": "{\"path\":\"shot.png\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": output
            }
        ]
    })
}

#[test]
fn tool_output_image_becomes_image_url_part_not_text() {
    let converted = responses_to_chat_completions(image_tool_call_input(json!([
        { "type": "input_image", "image_url": TEST_PNG_DATA_URL }
    ])))
    .unwrap();

    // tool 消息降级成字符串占位符——多数上游不接受 tool 消息带 multi-part 图片。
    let tool = &converted["messages"][2];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "call_1");
    assert_eq!(tool["content"], "[image]");

    // 图片改由紧随其后的 user 消息承载，且是结构化的 image_url。
    let carrier = &converted["messages"][3];
    assert_eq!(carrier["role"], "user");
    assert_eq!(carrier["content"][1]["type"], "image_url");
    assert_eq!(carrier["content"][1]["image_url"]["url"], TEST_PNG_DATA_URL);

    assert_no_base64_in_text(&converted);
}

#[test]
fn tool_output_image_accepts_object_shaped_image_url() {
    let converted = responses_to_chat_completions(image_tool_call_input(json!([
        { "type": "input_image", "image_url": { "url": TEST_PNG_DATA_URL, "detail": "high" } }
    ])))
    .unwrap();

    let image = &converted["messages"][3]["content"][1];
    assert_eq!(image["type"], "image_url");
    assert_eq!(image["image_url"]["url"], TEST_PNG_DATA_URL);
    assert_eq!(image["image_url"]["detail"], "high");
    assert_no_base64_in_text(&converted);
}

#[test]
fn tool_output_mixed_text_and_image_keeps_both() {
    let converted = responses_to_chat_completions(image_tool_call_input(json!([
        { "type": "output_text", "text": "screenshot captured" },
        { "type": "input_image", "image_url": TEST_PNG_DATA_URL }
    ])))
    .unwrap();

    assert_eq!(
        converted["messages"][2]["content"],
        "screenshot captured\n[image]"
    );
    assert_eq!(
        converted["messages"][3]["content"][1]["image_url"]["url"],
        TEST_PNG_DATA_URL
    );
    assert_no_base64_in_text(&converted);
}

#[test]
fn tool_output_without_image_keeps_previous_string_shape() {
    // 回归护栏：无图路径必须与修复前逐字节一致。
    let plain = responses_to_chat_completions(image_tool_call_input(json!("result"))).unwrap();
    assert_eq!(plain["messages"][2]["content"], "result");
    assert_eq!(plain["messages"].as_array().unwrap().len(), 3);

    let structured = responses_to_chat_completions(image_tool_call_input(
        json!([{ "type": "output_text", "text": "hi" }]),
    ))
    .unwrap();
    assert_eq!(
        structured["messages"][2]["content"],
        "[{\"text\":\"hi\",\"type\":\"output_text\"}]"
    );
    assert_eq!(structured["messages"].as_array().unwrap().len(), 3);
}

#[test]
fn tool_output_images_do_not_break_tool_call_pairing() {
    // 两个并行 tool call 各返回一张图。图片消息若插在两条 tool 消息之间，
    // enforce_tool_call_pairing 的 take_while(role=="tool") 会漏掉第二条，
    // 导致 call_2 被误判成 orphaned 而摘掉。
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash-vision-exp",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "compare these" }]
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "view_image",
                "arguments": "{\"path\":\"a.png\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_2",
                "name": "view_image",
                "arguments": "{\"path\":\"b.png\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": [{ "type": "input_image", "image_url": TEST_PNG_DATA_URL }]
            },
            {
                "type": "function_call_output",
                "call_id": "call_2",
                "output": [{ "type": "input_image", "image_url": TEST_PNG_DATA_URL }]
            },
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "well?" }]
            }
        ]
    }))
    .unwrap();

    let messages = converted["messages"].as_array().unwrap();

    // 两个 tool_call 都保住了，没有被当成 orphaned 摘掉。
    let tool_calls = messages[1]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0]["id"], "call_1");
    assert_eq!(tool_calls[1]["id"], "call_2");

    // 两条 tool 消息紧邻，中间没有被图片消息割开。
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "call_1");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_2");

    // 两张图汇总到连续 tool 区之后的一条 user 消息里。
    assert_eq!(messages[4]["role"], "user");
    let parts = messages[4]["content"].as_array().unwrap();
    let images = parts
        .iter()
        .filter(|part| part["type"] == "image_url")
        .count();
    assert_eq!(images, 2);

    assert_no_base64_in_text(&converted);
}

#[test]
fn orphan_tool_output_image_stays_inline_in_user_message() {
    // 没有配对 function_call 的 output 会降级成 user 消息；它本就是 user 角色，
    // 图片可以直接内联，不必再搬一次。
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash-vision-exp",
        "input": [
            {
                "type": "function_call_output",
                "call_id": "call_orphan",
                "output": [{ "type": "input_image", "image_url": TEST_PNG_DATA_URL }]
            }
        ]
    }))
    .unwrap();

    let message = &converted["messages"][0];
    assert_eq!(message["role"], "user");
    assert!(
        message["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("call_orphan")
    );
    assert_eq!(message["content"][1]["type"], "image_url");
    assert_eq!(message["content"][1]["image_url"]["url"], TEST_PNG_DATA_URL);
    assert_no_base64_in_text(&converted);
}

#[test]
fn custom_tool_call_output_image_is_also_converted() {
    let converted = responses_to_chat_completions(json!({
        "model": "deepseek-v4-flash-vision-exp",
        "input": [
            {
                "type": "custom_tool_call",
                "call_id": "call_1",
                "name": "snap",
                "input": "{}"
            },
            {
                "type": "custom_tool_call_output",
                "call_id": "call_1",
                "output": [{ "type": "input_image", "image_url": TEST_PNG_DATA_URL }]
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted["messages"][1]["role"], "tool");
    assert_eq!(converted["messages"][1]["content"], "[image]");
    assert_eq!(
        converted["messages"][2]["content"][1]["image_url"]["url"],
        TEST_PNG_DATA_URL
    );
    assert_no_base64_in_text(&converted);
}

#[test]
fn empty_image_url_is_dropped_rather_than_forwarded() {
    let converted = responses_to_chat_completions(image_tool_call_input(json!([
        { "type": "input_image", "image_url": "" }
    ])))
    .unwrap();

    // 空 url 不值得转发，也不该留下空的 image_url part。
    let messages = converted["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["role"], "tool");
}

// ---------------------------------------------------------------------------
// 2026-09-16 审查回归测试（R03/R05）。先在未修复代码上跑红，作为修复前证据。
// ---------------------------------------------------------------------------

// T04 / R05：顶层扁平名与 namespace 展开名冲突时必须 fail closed，
// 在请求发往上游之前报错；mock 上游请求数必须为 0。
#[tokio::test]
async fn namespace_flatten_conflict_with_top_level_tool_fails_closed_before_upstream() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut received = 0usize;
        while let Ok((mut stream, _)) = listener.accept().await {
            received += 1;
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).await.unwrap();
            let _ = stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 35\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp_1\",\"object\":\"response\"}",
                )
                .await;
        }
        received
    });
    let mut settings = aggregate_proxy_settings(
        "conflict-fail-closed",
        format!("http://{addr}/v1"),
        format!("http://{addr}/v1"),
    );
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let request = json!({
        "model": "gpt-5-mini",
        "stream": false,
        "tools": [
            {"type": "function", "name": "fs__read", "parameters": {"type": "object"}},
            {"type": "namespace", "name": "fs", "tools": [
                {"type": "function", "name": "read", "parameters": {"type": "object"}}
            ]}
        ],
        "input": "hi"
    });
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings).await;
    assert!(
        result.is_err(),
        "扁平名冲突必须在发送上游前报错，而不是静默覆盖工具身份"
    );
    server.abort();
    let received = server.await.unwrap_or(0);
    assert_eq!(
        received, 0,
        "冲突请求不能发往上游（实际收到 {received} 个请求）"
    );
}

// A01 / R03：候选 A（native interop on）429 后失败转移到候选 B（interop off）时，
// B 收到的请求不能继承 A 的原生子任务别名/明文标记改写。
#[tokio::test]
async fn aggregate_fallback_does_not_inherit_first_candidate_native_agent_rewrites() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}",
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));
    let mut settings = aggregate_proxy_settings(
        "native-isolation-off",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[0].native_agent_interop = NativeAgentInterop::On;
    settings.relay_profiles[1].native_agent_interop = NativeAgentInterop::Off;
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let request = json!({
        "model": "gpt-5-mini",
        "stream": false,
        "tools": [{
            "type": "namespace", "name": "collaboration", "tools": [
                {"type": "function", "name": "spawn_agent", "parameters": {
                    "type": "object",
                    "properties": {"message": {"type": "string"}}
                }}
            ]
        }],
        "input": [
            {"type": "message", "role": "user", "content": "fixture-task"},
            {"type": "agent_message", "content": [
                {"type": "input_text", "text": "plaintext task"}
            ]}
        ]
    });
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let _ = result.response.bytes().await.unwrap();

    let (_, second_body) = second_server.await.unwrap();
    let tools = second_body["tools"].as_array().unwrap();
    // 通用 namespace 扁平化仍会按 B 自己的策略展开工具，但展开名不能带 A 的原生别名前缀。
    assert!(
        tools.iter().all(|tool| !tool
            .get("name")
            .and_then(|name| name.as_str())
            .is_some_and(|name| name.starts_with("codexpp_native_collaboration"))),
        "候选 B（interop off）不能继承候选 A 的 collaboration 别名改写，实际 tools: {tools:?}"
    );
    assert_eq!(
        tools[0]["name"],
        json!("collaboration__spawn_agent"),
        "候选 B 按自己的策略执行通用扁平化（无原生别名）"
    );
    let message_property = tools
        .iter()
        .flat_map(|tool| tool["parameters"]["properties"]["message"].as_object())
        .next();
    assert!(
        !message_property.is_some_and(|property| property.get("encrypted") == Some(&json!(false))),
        "候选 B 不能继承候选 A 的 encrypted:false 明文标记"
    );
    assert_eq!(
        second_body["input"][1]["type"], "agent_message",
        "候选 B（interop off）不能继承候选 A 的 agent_message 重建行为"
    );
}

// A02 / R03：候选 A（interop off）429 后转移到候选 B（interop on）时，
// B 必须完整执行自己的预处理，且响应恢复使用 B 自己的上下文。
#[tokio::test]
async fn aggregate_fallback_second_candidate_applies_its_own_native_prep_and_restore() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}",
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));
    let mut settings = aggregate_proxy_settings(
        "native-isolation-on",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[0].native_agent_interop = NativeAgentInterop::Off;
    settings.relay_profiles[1].native_agent_interop = NativeAgentInterop::On;
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let request = json!({
        "model": "gpt-5-mini",
        "stream": false,
        "tools": [{
            "type": "namespace", "name": "collaboration", "tools": [
                {"type": "function", "name": "spawn_agent", "parameters": {
                    "type": "object",
                    "properties": {"message": {"type": "string"}}
                }}
            ]
        }],
        "input": [{"type": "message", "role": "user", "content": "fixture-task"}]
    });
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);

    let (_, second_body) = second_server.await.unwrap();
    // B 的预处理先生效（collaboration -> codexpp_native_collaboration），
    // 随后通用扁平化把 namespace 展开成子工具扁平名。
    assert_eq!(
        second_body["tools"][0]["name"],
        json!("codexpp_native_collaboration__spawn_agent"),
        "候选 B（interop on）必须执行自己的 collaboration 别名预处理"
    );
    assert_eq!(
        second_body["tools"][0]["parameters"]["properties"]["message"]["encrypted"],
        json!(false),
        "候选 B 必须执行自己的明文标记预处理"
    );
}

// A04 / R03+R12：两候选配置不同模型别名时，B 的物理模型必须来自 B 自己的配置，
// 不能继承 A 的别名改写结果。
#[tokio::test]
async fn aggregate_fallback_resolves_each_candidate_model_alias_independently() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}",
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));
    let mut settings = aggregate_proxy_settings(
        "alias-isolation",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[0].model_aliases = vec![codex_plus_core::settings::RelayModelAlias {
        alias: "logical-model".to_string(),
        model: "physical-a".to_string(),
    }];
    settings.relay_profiles[1].model_aliases = vec![codex_plus_core::settings::RelayModelAlias {
        alias: "logical-model".to_string(),
        model: "physical-b".to_string(),
    }];
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let result = open_responses_proxy_request_with_settings(
        r#"{"model":"logical-model","input":"hi","stream":false}"#,
        settings,
    )
    .await
    .unwrap();
    assert_eq!(result.status_code, 200);
    let _ = result.response.bytes().await.unwrap();

    let first_request = first_server.await.unwrap();
    let first_body = first_request
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str::<serde_json::Value>(body).ok())
        .unwrap();
    assert_eq!(first_body["model"], json!("physical-a"));
    let (_, second_body) = second_server.await.unwrap();
    assert_eq!(
        second_body["model"],
        json!("physical-b"),
        "候选 B 的物理模型必须来自 B 自己的别名配置，而不是 A 的改写结果"
    );
}

// A08 / R03：整个 fallback 过程结束后，调用方传入的原始请求体不能被原地修改。
#[tokio::test]
async fn aggregate_fallback_leaves_original_request_json_unmodified() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_request_and_respond_once(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}",
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));
    let mut settings = aggregate_proxy_settings(
        "original-unmodified",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[0].native_agent_interop = NativeAgentInterop::On;
    settings.relay_profiles[0].model_aliases = vec![codex_plus_core::settings::RelayModelAlias {
        alias: "gpt-5-mini".to_string(),
        model: "physical-a".to_string(),
    }];
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let original = json!({
        "model": "gpt-5-mini",
        "stream": false,
        "tools": [{
            "type": "namespace", "name": "collaboration", "tools": [
                {"type": "function", "name": "spawn_agent", "parameters": {
                    "type": "object",
                    "properties": {"message": {"type": "string"}}
                }}
            ]
        }],
        "input": [{"type": "message", "role": "user", "content": "fixture-task"}]
    });
    let original_snapshot = original.clone();
    let result = open_responses_proxy_request_with_settings(&original.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let _ = result.response.bytes().await.unwrap();
    first_server.await.unwrap();
    second_server.await.unwrap();

    // 原始 JSON 字符串本身不含任何候选改写痕迹（别名/别名扁平名/明文标记）。
    let serialized = original_snapshot.to_string();
    assert!(
        !serialized.contains("physical-a")
            && !serialized.contains("codexpp_native_collaboration")
            && !serialized.contains("encrypted"),
        "原始请求不能被候选改写污染：{serialized}"
    );
}

// ===========================================================================
// Custom-as-Function 双向适配器（customToolsAsFunctions，默认关闭）
// ===========================================================================

/// 可指定响应的 JSON 请求捕获（供 fallback 与响应还原测试复用）。
async fn capture_json_request_and_respond(
    listener: tokio::net::TcpListener,
    response: String,
) -> (String, serde_json::Value) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request closed before headers completed");
        buffer.extend_from_slice(&chunk[..read]);
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            })
            .unwrap_or(0);
        break (header_end + 4, content_length);
    };
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request closed before body completed");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let headers = String::from_utf8_lossy(&buffer[..header_end - 4]).to_string();
    let body = serde_json::from_slice(&buffer[header_end..header_end + content_length]).unwrap();
    stream.write_all(response.as_bytes()).await.unwrap();
    (headers, body)
}

/// canonical custom fixture：namespace 内 custom + 普通 function、顶层 custom、
/// 一轮 custom 调用/结果历史、tool_choice 指向 namespace custom。
fn custom_adapter_request_fixture() -> serde_json::Value {
    json!({
        "model": "review-model",
        "stream": false,
        "tool_choice": { "type": "custom", "namespace": "functions", "name": "review_echo" },
        "tools": [
            {
                "type": "namespace", "name": "functions", "tools": [
                    {
                        "type": "custom", "name": "review_echo",
                        "description": "Echo exact input; this is a safe test tool, not a shell.",
                        "format": { "type": "text" }
                    },
                    {
                        "type": "function", "name": "review_wait",
                        "description": "A normal function control tool.",
                        "parameters": {
                            "type": "object",
                            "properties": { "token": { "type": "string" } },
                            "required": ["token"],
                            "additionalProperties": false
                        },
                        "strict": false
                    }
                ]
            },
            {
                "type": "custom", "name": "review_echo",
                "description": "Top-level echo tool with the same client name.",
                "format": { "type": "text" }
            }
        ],
        "input": [
            { "type": "message", "role": "user", "content": "Use review_echo with the exact test input requested by the harness." },
            {
                "type": "custom_tool_call", "id": "ctc_hist1", "call_id": "call_hist1",
                "name": "review_echo",
                "input": "第一行\n  第二行\\路径\"引号\"  "
            },
            {
                "type": "custom_tool_call_output", "call_id": "call_hist1",
                "id": "ctco_hist1",
                "output": "工具结果占位"
            }
        ]
    })
}

fn http_json_response(status_line: &str, body: &serde_json::Value) -> String {
    let text = body.to_string();
    format!(
        "{status_line}\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{text}",
        text.len()
    )
}

/// C01（回归护栏）：开关关闭时 wire 保持 custom 原形，普通请求不受影响。
#[tokio::test]
async fn custom_adapter_off_keeps_custom_wire_form() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(capture_json_request_once(listener));
    let settings = BackendSettings {
        active_relay_id: "adapter-off".to_string(),
        relay_profiles: vec![RelayProfile {
            id: "adapter-off".to_string(),
            name: "off".to_string(),
            base_url: format!("http://{addr}/v1"),
            api_key: "sk-test".to_string(),
            protocol: RelayProtocol::Responses,
            relay_mode: RelayMode::PureApi,
            custom_tools_as_functions: false,
            ..RelayProfile::default()
        }],
        ..BackendSettings::default()
    };

    let request = custom_adapter_request_fixture();
    let original = request.clone();
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, body) = server.await.unwrap();

    // namespace 扁平化后 custom 仍是 custom；普通 function 原样。
    let wire_tools = body["tools"].as_array().unwrap();
    let echo = wire_tools
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .expect("namespace custom 应被扁平化保留");
    assert_eq!(echo["type"], "custom", "关闭开关时不得包装成 function");
    assert_eq!(echo["format"]["type"], "text");
    // 历史 custom 调用保持原类型（仅做 ID 规范化）。
    let history_call = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["call_id"] == "call_hist1")
        .unwrap();
    assert_eq!(history_call["type"], "custom_tool_call");
    assert_eq!(history_call["input"], "第一行\n  第二行\\路径\"引号\"  ");
    // 原始请求未被原地改写（H07）。
    assert_eq!(request, original);
}

/// Q01/Q02/Q05/H01/H02（开关开启，Responses 上游）：声明/历史/tool_choice
/// 全部按统一计划包装，普通 function 与顶层身份不受影响，原始请求不改写。
#[tokio::test]
async fn custom_adapter_on_wraps_declarations_history_and_tool_choice() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(capture_json_request_once(listener));
    let settings = BackendSettings {
        active_relay_id: "adapter-on".to_string(),
        relay_profiles: vec![RelayProfile {
            id: "adapter-on".to_string(),
            name: "on".to_string(),
            base_url: format!("http://{addr}/v1"),
            api_key: "sk-test".to_string(),
            protocol: RelayProtocol::Responses,
            relay_mode: RelayMode::PureApi,
            custom_tools_as_functions: true,
            ..RelayProfile::default()
        }],
        ..BackendSettings::default()
    };

    let request = custom_adapter_request_fixture();
    let original = request.clone();
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, body) = server.await.unwrap();

    let wire_tools = body["tools"].as_array().unwrap();
    let echo = wire_tools
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .expect("namespace custom 必须以扁平 function 形式发送");
    assert_eq!(echo["type"], "function");
    assert_eq!(echo["strict"], json!(false));
    assert_eq!(echo["parameters"]["type"], "object");
    assert_eq!(echo["parameters"]["properties"]["input"]["type"], "string");
    assert_eq!(echo["parameters"]["required"], json!(["input"]));
    assert_eq!(echo["parameters"]["additionalProperties"], json!(false));
    assert!(
        echo["description"]
            .as_str()
            .unwrap()
            .contains("Echo exact input"),
        "原描述必须并入包装描述"
    );

    let wait = wire_tools
        .iter()
        .find(|tool| tool["name"] == "functions__review_wait")
        .expect("普通 function 保持扁平名发送");
    assert_eq!(wait["type"], "function");
    assert!(wait.get("strict").is_none() || wait["strict"] == json!(false));
    assert!(
        wait["parameters"]["properties"]["token"].is_object(),
        "普通 function 的参数 schema 不得被适配器改写"
    );

    let top = wire_tools
        .iter()
        .find(|tool| tool["name"] == "review_echo" && tool["type"] == "function")
        .expect("顶层 custom 以原 wire 名包装");
    assert_eq!(top["parameters"]["required"], json!(["input"]));

    let items = body["input"].as_array().unwrap();
    let history_call = items
        .iter()
        .find(|item| item["call_id"] == "call_hist1")
        .unwrap();
    assert_eq!(history_call["type"], "function_call");
    assert_eq!(
        history_call["name"], "review_echo",
        "历史调用按客户端原名包装"
    );
    assert_eq!(
        history_call["id"], "fc_hist1",
        "包装为 function_call 后 item id 必须符合上游 fc_ 前缀校验"
    );
    let arguments = history_call["arguments"].as_str().unwrap();
    let decoded: serde_json::Value = serde_json::from_str(arguments).unwrap();
    assert_eq!(
        decoded["input"], "第一行\n  第二行\\路径\"引号\"  ",
        "装箱只包一层 JSON，input 字符串逐字符保留"
    );
    let history_output = items
        .iter()
        .find(|item| item["call_id"] == "call_hist1" && item["type"] == "function_call_output")
        .expect("结果与调用一起转换");
    assert_eq!(
        history_output["id"], "fco_hist1",
        "包装输出也必须符合上游 function_call_output 的 fco_ 前缀校验"
    );
    assert_eq!(
        history_output["output"], "工具结果占位",
        "output 不做二次封装"
    );

    assert_eq!(
        body["tool_choice"],
        json!({ "type": "function", "name": "functions__review_echo" })
    );
    assert_eq!(request, original, "原始请求对象不得被原地改写");
}

/// 官方 DeepSeek 的 profile 默认未打开 customToolsAsFunctions，但其 Responses
/// 端点不接受 Code Mode 的 custom 工具。代理必须自动复用同一双向 function 适配，
/// 同时把历史 message id 修成 Responses 要求的 msg_ 命名空间。
#[tokio::test]
async fn official_deepseek_auto_wraps_custom_tools_and_normalizes_history_ids() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(capture_json_request_once(listener));
    let settings = BackendSettings {
        active_relay_id: "official-deepseek".to_string(),
        relay_profiles: vec![RelayProfile {
            id: "official-deepseek".to_string(),
            name: "DeepSeek".to_string(),
            base_url: format!("http://{addr}/v1"),
            upstream_base_url: "https://api.deepseek.com/".to_string(),
            config_contents: format!(
                "model = \"deepseek-v4-flash\"\nmodel_provider = \"custom\"\n\n[model_providers.custom]\nwire_api = \"responses\"\nbase_url = \"http://{addr}/v1\"\n"
            ),
            api_key: "sk-test".to_string(),
            protocol: RelayProtocol::Responses,
            relay_mode: RelayMode::PureApi,
            custom_tools_as_functions: false,
            ..RelayProfile::default()
        }],
        ..BackendSettings::default()
    };

    let mut request = custom_adapter_request_fixture();
    request["input"][0]["id"] = json!("chatcmpl-202609200918258761407968268d9d6WGd5KQKm_msg_0");
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let (_, body) = server.await.unwrap();

    let message = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "message")
        .unwrap();
    assert!(
        message["id"].as_str().unwrap().starts_with("msg_"),
        "官方 DeepSeek 不接受 chatcmpl_*_msg_* 历史 id"
    );
    let echo = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .unwrap();
    assert_eq!(echo["type"], "function");
    let history_call = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["call_id"] == "call_hist1")
        .unwrap();
    assert_eq!(history_call["type"], "function_call");
    assert!(history_call["id"].as_str().unwrap().starts_with("fc_"));
}

/// J01/J03/J06 + §10.4（JSON 返回入口，端到端）：function_call 精确还原为
/// custom_tool_call，普通 function 不动，回显 tools/tool_choice 还原为客户端视图。
#[tokio::test]
async fn custom_adapter_json_response_restores_custom_tool_call_end_to_end() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    let upstream_response = json!({
        "id": "resp_adapter",
        "object": "response",
        "status": "completed",
        "output": [
            {
                "type": "function_call", "id": "fc_ns", "call_id": "call_ns",
                "name": "functions__review_echo",
                "arguments": serde_json::to_string(&json!({ "input": "line1\r\nline2 \"quoted\" 🧪" })).unwrap(),
                "status": "completed"
            },
            {
                "type": "function_call", "id": "fc_plain", "call_id": "call_plain",
                "name": "functions__review_wait",
                "arguments": serde_json::to_string(&json!({ "token": "t" })).unwrap(),
                "status": "completed"
            },
            {
                "type": "function_call", "id": "fc_top", "call_id": "call_top",
                "name": "review_echo",
                "arguments": serde_json::to_string(&json!({ "input": "" })).unwrap(),
                "status": "completed"
            }
        ],
        "tools": [
            { "type": "function", "name": "functions__review_echo", "description": "wrapped" },
            { "type": "function", "name": "review_echo", "description": "wrapped-top" }
        ],
        "tool_choice": { "type": "function", "name": "functions__review_echo" }
    });
    let server = tokio::spawn(capture_json_request_and_respond(
        listener,
        http_json_response("HTTP/1.1 200 OK", &upstream_response),
    ));

    let settings = json!({
        "relayProfiles": [{
            "id": "adapter-json",
            "name": "adapter",
            "baseUrl": format!("http://{addr}/v1"),
            "upstreamBaseUrl": format!("http://{addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "responses",
            "relayMode": "pureApi",
            "customToolsAsFunctions": true
        }],
        "activeRelayId": "adapter-json"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let request = json!({
        "model": "review-model",
        "stream": false,
        "tools": [
            {
                "type": "namespace", "name": "functions", "tools": [
                    { "type": "custom", "name": "review_echo", "description": "Echo tool.", "format": { "type": "text" } },
                    { "type": "function", "name": "review_wait", "parameters": { "type": "object", "properties": { "token": { "type": "string" } } } }
                ]
            },
            { "type": "custom", "name": "review_echo", "description": "Top-level echo tool with the same client name.", "format": { "type": "text" } }
        ],
        "input": [{ "type": "message", "role": "user", "content": "go" }]
    });
    let response =
        codex_plus_core::protocol_proxy::handle_responses_proxy_request(&request.to_string())
            .await
            .unwrap();
    assert_eq!(response.status, "200 OK");
    let _ = server.await.unwrap();

    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let output = body["output"].as_array().unwrap();

    assert_eq!(output[0]["type"], "custom_tool_call");
    assert_eq!(output[0]["name"], "review_echo");
    assert_eq!(output[0]["namespace"], "functions");
    assert_eq!(output[0]["id"], "ctc_ns");
    assert_eq!(output[0]["call_id"], "call_ns");
    assert_eq!(
        output[0]["input"], "line1\r\nline2 \"quoted\" 🧪",
        "input 解码后必须逐字符相等"
    );
    assert!(
        output[0].get("arguments").is_none(),
        "function 专属字段必须移除"
    );

    assert_eq!(output[1]["type"], "function_call", "普通 function 不误转");
    // 既有 namespace 还原管线把扁平名恢复为客户端视图（原名 + namespace 字段）。
    assert_eq!(output[1]["name"], "review_wait");
    assert_eq!(output[1]["namespace"], "functions");
    assert_eq!(output[1]["arguments"], "{\"token\":\"t\"}");

    assert_eq!(output[2]["type"], "custom_tool_call");
    assert!(
        output[2].get("namespace").is_none(),
        "顶层 custom 不携带 namespace"
    );
    assert_eq!(output[2]["input"], "", "合法空串不得被丢弃");

    let echoed_tools = body["tools"].as_array().unwrap();
    assert_eq!(echoed_tools[0]["type"], "custom", "回显声明还原为 custom");
    assert_eq!(echoed_tools[0]["name"], "review_echo");
    assert_eq!(
        body["tool_choice"],
        json!({ "type": "custom", "namespace": "functions", "name": "review_echo" })
    );
}

/// C03 / §4.3：passthrough + 开关是配置冲突，发送前显式失败，上游收到 0 个请求。
#[tokio::test]
async fn custom_adapter_conflicts_with_passthrough_and_fails_before_upstream() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    // 非阻塞探测：若适配器错误地把请求发往上游，accept 会立即成功。
    let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();

    let settings = json!({
        "relayProfiles": [{
            "id": "adapter-conflict",
            "name": "conflict",
            "baseUrl": format!("http://{addr}/v1"),
            "upstreamBaseUrl": format!("http://{addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "responses",
            "relayMode": "pureApi",
            "responsesWirePolicy": "passthrough",
            "customToolsAsFunctions": true
        }],
        "activeRelayId": "adapter-conflict"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let request = custom_adapter_request_fixture();
    // 缓冲入口把错误作为 Err 传播（launcher 层渲染为 502 + 错误体）。
    let error =
        codex_plus_core::protocol_proxy::handle_responses_proxy_request(&request.to_string())
            .await
            .unwrap_err();
    assert!(
        error.to_string().contains("CUSTOM_ADAPTER_POLICY_CONFLICT"),
        "错误必须携带类型化错误码：{error}"
    );
    probe.set_nonblocking(true).unwrap();
    assert!(probe.accept().is_err(), "冲突请求不得发往上游");
}

/// §9.3：开关开启时依赖服务端会话状态的请求必须显式报不支持。
#[tokio::test]
async fn custom_adapter_rejects_server_state_references() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    // 非阻塞探测：若适配器错误地把请求发往上游，accept 会立即成功。
    let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();

    let settings = json!({
        "relayProfiles": [{
            "id": "adapter-ref",
            "name": "ref",
            "baseUrl": format!("http://{addr}/v1"),
            "upstreamBaseUrl": format!("http://{addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "responses",
            "relayMode": "pureApi",
            "customToolsAsFunctions": true
        }],
        "activeRelayId": "adapter-ref"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let request = json!({
        "model": "review-model",
        "previous_response_id": "resp_prev",
        "tools": [{ "type": "custom", "name": "review_echo" }],
        "input": [{ "type": "message", "role": "user", "content": "continue" }]
    });
    let error =
        codex_plus_core::protocol_proxy::handle_responses_proxy_request(&request.to_string())
            .await
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("CUSTOM_ADAPTER_STATE_REFERENCE_UNSUPPORTED"),
        "服务端引用必须显式报不支持：{error}"
    );
    probe.set_nonblocking(true).unwrap();
    assert!(probe.accept().is_err(), "带服务端引用的请求不得发往上游");
}

/// F01（候选隔离）：A 开关开 → 429 → B 开关关。A 看到 function 包装，
/// B 看到该协议本来应有的 custom 形态，互不污染。
#[tokio::test]
async fn custom_adapter_fallback_from_enabled_to_disabled_is_isolated() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_json_request_and_respond(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}".to_string(),
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));

    let mut settings = aggregate_proxy_settings(
        "adapter-iso-ab",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[0].custom_tools_as_functions = true;
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let request = custom_adapter_request_fixture();
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);

    let (_, first_body) = first_server.await.unwrap();
    let a_echo = first_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .unwrap();
    assert_eq!(
        a_echo["type"], "function",
        "A（开关开）必须看到 function 包装"
    );

    let (_, second_body) = second_server.await.unwrap();
    let b_echo = second_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .unwrap();
    assert_eq!(b_echo["type"], "custom", "B（开关关）必须保持 custom 形态");
    let b_history = second_body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["call_id"] == "call_hist1")
        .unwrap();
    assert_eq!(
        b_history["type"], "custom_tool_call",
        "B 不继承 A 的历史转换"
    );
}

/// F02（候选隔离反向）：A 开关关 → 429 → B 开关开。B 独立完成包装。
#[tokio::test]
async fn custom_adapter_fallback_from_disabled_to_enabled_is_isolated() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let first = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let first_addr = first.local_addr().unwrap();
    let second = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let second_addr = second.local_addr().unwrap();
    let first_server = tokio::spawn(capture_json_request_and_respond(
        first,
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 13\r\ncontent-type: application/json\r\n\r\n{\"error\":429}".to_string(),
    ));
    let second_server = tokio::spawn(capture_json_request_once(second));

    let mut settings = aggregate_proxy_settings(
        "adapter-iso-ba",
        format!("http://{first_addr}/v1"),
        format!("http://{second_addr}/v1"),
    );
    settings.relay_profiles[1].custom_tools_as_functions = true;
    for relay in settings.relay_profiles.iter_mut().take(2) {
        relay.relay_mode = RelayMode::PureApi;
        relay.no_auth = true;
        relay.api_key.clear();
    }

    let request = custom_adapter_request_fixture();
    let result = open_responses_proxy_request_with_settings(&request.to_string(), settings)
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);

    let (_, first_body) = first_server.await.unwrap();
    let a_echo = first_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .unwrap();
    assert_eq!(a_echo["type"], "custom", "A（开关关）保持 custom 形态");

    let (_, second_body) = second_server.await.unwrap();
    let b_echo = second_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "functions__review_echo")
        .unwrap();
    assert_eq!(b_echo["type"], "function", "B（开关开）独立完成包装");
    let b_history = second_body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["call_id"] == "call_hist1")
        .unwrap();
    assert_eq!(b_history["type"], "function_call", "B 独立转换历史");
}

/// P01（完整 SSE 缓冲入口）：受管调用按调用缓冲，完整校验后交付 custom 事件组；
/// 普通事件原样流动，sequence_number 单调递增。
#[tokio::test]
async fn custom_adapter_buffered_sse_restores_custom_events() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    let sse_body = format!(
        "event: response.created\ndata: {}\n\n\
         event: response.output_item.added\ndata: {}\n\n\
         event: response.function_call_arguments.delta\ndata: {}\n\n\
         event: response.function_call_arguments.done\ndata: {}\n\n\
         event: response.output_item.done\ndata: {}\n\n\
         event: response.completed\ndata: {}\n\n",
        json!({ "type": "response.created", "response": { "id": "resp_sse" } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "id": "fc_9", "type": "function_call", "call_id": "call_sse", "name": "review_echo", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_9", "output_index": 0, "delta": "{\"inp" }),
        json!({ "type": "response.function_call_arguments.done", "item_id": "fc_9", "output_index": 0, "arguments": "{\"input\":\"流式 🧪\"}" }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "id": "fc_9", "type": "function_call", "call_id": "call_sse", "name": "review_echo", "arguments": "{\"input\":\"流式 🧪\"}", "status": "completed" } }),
        json!({ "type": "response.completed", "response": { "id": "resp_sse", "status": "completed", "output": [ { "type": "function_call", "id": "fc_9", "call_id": "call_sse", "name": "review_echo", "arguments": "{\"input\":\"流式 🧪\"}", "status": "completed" } ] } }),
    );
    let response_text = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: text/event-stream\r\n\r\n{sse_body}",
        sse_body.len()
    );
    let listener2 = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let _ = listener2.local_addr().unwrap();
    let server = tokio::spawn(capture_json_request_and_respond(listener, response_text));

    let settings = json!({
        "relayProfiles": [{
            "id": "adapter-sse",
            "name": "sse",
            "baseUrl": format!("http://{addr}/v1"),
            "upstreamBaseUrl": format!("http://{addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "responses",
            "relayMode": "pureApi",
            "customToolsAsFunctions": true
        }],
        "activeRelayId": "adapter-sse"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let request = json!({
        "model": "review-model",
        "stream": true,
        "tools": [{ "type": "custom", "name": "review_echo", "description": "Echo tool." }],
        "input": [{ "type": "message", "role": "user", "content": "go" }]
    });
    let response =
        codex_plus_core::protocol_proxy::handle_responses_proxy_request(&request.to_string())
            .await
            .unwrap();
    assert_eq!(response.status, "200 OK");
    let _ = server.await.unwrap();

    let text = String::from_utf8(response.body).unwrap();
    assert!(
        text.contains("response.custom_tool_call_input.delta"),
        "必须交付 custom input delta 事件：{text}"
    );
    assert!(text.contains("response.custom_tool_call_input.done"));
    assert!(text.contains("\"input\":\"流式 🧪\""));
    assert!(
        !text.contains("response.function_call_arguments"),
        "受管调用的 function arguments 事件必须被吸收"
    );
    assert!(text.contains("\"call_id\":\"call_sse\""));
    // sequence_number 单调。
    let mut sequences = Vec::new();
    for frame in text.split("\n\n") {
        if let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(sequence) = value.get("sequence_number").and_then(|v| v.as_u64()) {
                    sequences.push(sequence);
                }
            }
        }
    }
    let mut sorted = sequences.clone();
    sorted.sort();
    assert_eq!(sequences, sorted, "sequence_number 必须单调递增");
}

/// P02（Chat 上游 + 开关开启，端到端）：namespace custom 被包装为扁平 function
/// 兼容网关省略 reasoning item 的 summary 注册（真实 UnoRouter/GPT-5.6-Luna 形状）时，
/// 缓冲入口必须先补齐 item 与 summary 注册，否则 Codex 会报
/// ReasoningSummaryDelta without active item 并丢掉整轮输出。
#[tokio::test]
async fn responses_reasoning_frames_register_summary_before_delta_end_to_end() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    let sse_body = format!(
        "event: response.created\ndata: {}\n\n\
         event: response.output_item.added\ndata: {}\n\n\
         event: response.reasoning_summary_text.delta\ndata: {}\n\n\
         event: response.output_item.done\ndata: {}\n\n\
         event: response.completed\ndata: {}\n\n",
        json!({ "type": "response.created", "response": { "id": "resp_reason" } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "id": "chatcmpl-x_reasoning_0", "type": "reasoning", "status": "in_progress" } }),
        json!({ "type": "response.reasoning_summary_text.delta", "item_id": "chatcmpl-x_reasoning_0", "output_index": 0, "delta": "thinking" }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "id": "chatcmpl-x_reasoning_0", "type": "reasoning", "status": "completed" } }),
        json!({ "type": "response.completed", "response": { "id": "resp_reason", "status": "completed", "output": [] } }),
    );
    let response_text = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: text/event-stream\r\n\r\n{sse_body}",
        sse_body.len()
    );
    let server = tokio::spawn(capture_json_request_and_respond(listener, response_text));

    let settings = json!({
        "relayProfiles": [{
            "id": "reasoning-sse",
            "name": "sse",
            "baseUrl": format!("http://{addr}/v1"),
            "upstreamBaseUrl": format!("http://{addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "responses",
            "relayMode": "pureApi"
        }],
        "activeRelayId": "reasoning-sse"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let request = json!({
        "model": "review-model",
        "stream": true,
        "input": [{ "type": "message", "role": "user", "content": "go" }]
    });
    let response =
        codex_plus_core::protocol_proxy::handle_responses_proxy_request(&request.to_string())
            .await
            .unwrap();
    assert_eq!(response.status, "200 OK");
    let _ = server.await.unwrap();

    let text = String::from_utf8(response.body).unwrap();
    let kinds: Vec<String> = text
        .split("\n\n")
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        })
        .filter_map(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let delta_at = kinds
        .iter()
        .position(|kind| kind == "response.reasoning_summary_text.delta")
        .expect("delta 必须存在");
    let part_added_at = kinds
        .iter()
        .position(|kind| kind == "response.reasoning_summary_part.added")
        .expect("summary 注册帧必须被补齐");
    let item_added_at = kinds
        .iter()
        .position(|kind| kind == "response.output_item.added")
        .expect("reasoning item 注册帧必须存在");
    assert!(
        item_added_at < part_added_at && part_added_at < delta_at,
        "注册顺序必须是 item.added → part.added → delta，实际 {kinds:?}"
    );
    assert!(
        text.contains("\"summary\":[]"),
        "reasoning item 必须带 summary 字段：{text}"
    );
}

/// P02（Chat 上游 + 开关开启，端到端）：namespace custom 被包装为扁平 function
/// 发送，响应还原为带 namespace 的 custom_tool_call；顶层 custom 走既有转换。
#[tokio::test]
async fn custom_adapter_chat_upstream_restores_namespace_custom_end_to_end() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    let chat_response = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "review-model",
        "choices": [{
            "index": 0,
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_chat1",
                    "type": "function",
                    "function": {
                        "name": "functions__review_echo",
                        "arguments": serde_json::to_string(&json!({ "input": "chat 🧪" })).unwrap()
                    }
                }]
            }
        }]
    });
    let server = tokio::spawn(capture_json_request_and_respond(
        listener,
        http_json_response("HTTP/1.1 200 OK", &chat_response),
    ));

    let settings = json!({
        "relayProfiles": [{
            "id": "adapter-chat",
            "name": "chat",
            "baseUrl": format!("http://{addr}/v1"),
            "upstreamBaseUrl": format!("http://{addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "chatCompletions",
            "relayMode": "pureApi",
            "customToolsAsFunctions": true
        }],
        "activeRelayId": "adapter-chat"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let request = json!({
        "model": "review-model",
        "stream": false,
        "tools": [{
            "type": "namespace", "name": "functions", "tools": [
                { "type": "custom", "name": "review_echo", "description": "Echo tool." }
            ]
        }],
        "input": [{ "type": "message", "role": "user", "content": "go" }]
    });
    let response =
        codex_plus_core::protocol_proxy::handle_responses_proxy_request(&request.to_string())
            .await
            .unwrap();
    assert_eq!(response.status, "200 OK");

    let (_, upstream_request) = server.await.unwrap();
    let chat_tools = upstream_request["tools"].as_array().unwrap();
    let echo = chat_tools
        .iter()
        .find(|tool| tool["function"]["name"] == "functions__review_echo")
        .expect("namespace custom 必须被包装为扁平 chat function");
    assert_eq!(echo["type"], "function");
    assert_eq!(
        echo["function"]["parameters"]["properties"]["input"]["type"],
        "string"
    );

    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let item = body["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "custom_tool_call")
        .expect("chat function_call 必须还原为 custom_tool_call");
    assert_eq!(item["name"], "review_echo");
    assert_eq!(item["namespace"], "functions");
    assert_eq!(item["input"], "chat 🧪");
    assert_eq!(item["call_id"], "call_chat1");
}

/// P01-3（真实 socket 流式入口）：helper 端口 → 协议代理 → mock 上游分多次写入；
/// 客户端边读边收，受管调用在校验后以 custom 事件组交付，上游收到 function 包装。
#[tokio::test]
async fn custom_adapter_real_socket_streaming_delivers_custom_events() {
    let _lock = settings_path_test_lock().lock().unwrap();
    let temp = tempfile::tempdir().unwrap();

    // mock 上游：捕获请求并分 3 片写 SSE（在帧中间切开）。
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let upstream_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let (header_end, content_length) = loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "upstream closed before headers");
            buffer.extend_from_slice(&chunk[..read]);
            let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                })
                .unwrap_or(0);
            break (header_end + 4, content_length);
        };
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "upstream closed before body");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let request_body: serde_json::Value =
            serde_json::from_slice(&buffer[header_end..header_end + content_length]).unwrap();

        let sse_body = format!(
            "event: response.created\ndata: {}\n\n\
             event: response.output_item.added\ndata: {}\n\n\
             event: response.function_call_arguments.delta\ndata: {}\n\n\
             event: response.function_call_arguments.done\ndata: {}\n\n\
             event: response.output_item.done\ndata: {}\n\n\
             event: response.completed\ndata: {}\n\n",
            json!({ "type": "response.created", "response": { "id": "resp_sock" } }),
            json!({ "type": "response.output_item.added", "output_index": 0, "item": { "id": "fc_s1", "type": "function_call", "call_id": "call_sock", "name": "review_echo", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_s1", "output_index": 0, "delta": "{\"inp" }),
            json!({ "type": "response.function_call_arguments.done", "item_id": "fc_s1", "output_index": 0, "arguments": "{\"input\":\"socket 流式 🧪\"}" }),
            json!({ "type": "response.output_item.done", "output_index": 0, "item": { "id": "fc_s1", "type": "function_call", "call_id": "call_sock", "name": "review_echo", "arguments": "{\"input\":\"socket 流式 🧪\"}", "status": "completed" } }),
            json!({ "type": "response.completed", "response": { "id": "resp_sock", "status": "completed", "output": [ { "type": "function_call", "id": "fc_s1", "call_id": "call_sock", "name": "review_echo", "arguments": "{\"input\":\"socket 流式 🧪\"}", "status": "completed" } ] } }),
        );
        let mut response = format!(
            "HTTP/1.1 200 OK
content-length: {}
content-type: text/event-stream

",
            sse_body.len()
        )
        .into_bytes();
        response.extend_from_slice(sse_body.as_bytes());
        let bytes = &response[..];
        let (mid1, mid2) = (bytes.len() / 3, 2 * bytes.len() / 3);
        stream.write_all(&bytes[..mid1]).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        stream.write_all(&bytes[mid1..mid2]).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        stream.write_all(&bytes[mid2..]).await.unwrap();
        stream.flush().await.unwrap();
        // 保持连接片刻再关闭，避免客户端读到 RST。
        tokio::time::sleep(Duration::from_millis(80)).await;
        request_body
    });

    let settings = json!({
        "relayProfiles": [{
            "id": "adapter-sock",
            "name": "sock",
            "baseUrl": format!("http://{upstream_addr}/v1"),
            "upstreamBaseUrl": format!("http://{upstream_addr}/v1"),
            "apiKey": "sk-test",
            "protocol": "responses",
            "relayMode": "pureApi",
            "customToolsAsFunctions": true
        }],
        "activeRelayId": "adapter-sock"
    });
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let _guard = SettingsPathGuard::set(temp.path().join("settings.json"));

    let helper_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let helper_port = helper_listener.local_addr().unwrap().port();
    drop(helper_listener);
    let hooks = DefaultLaunchHooks::default();
    hooks.start_helper(helper_port).await.unwrap();

    let request_body = json!({
        "model": "review-model",
        "stream": true,
        "tools": [{ "type": "custom", "name": "review_echo", "description": "Echo tool." }],
        "input": [{ "type": "message", "role": "user", "content": "go" }]
    })
    .to_string();
    // 用 tokio 客户端：#[tokio::test] 默认单线程 runtime，
    // 阻塞式 std 读会饿死 helper 协程造成死锁。
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", helper_port))
        .await
        .unwrap();
    let http_request = format!(
        "POST /v1/responses HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        request_body.len(),
        request_body
    );
    client.write_all(http_request.as_bytes()).await.unwrap();

    let mut received = Vec::new();
    let mut reads = 0usize;
    loop {
        let mut chunk = [0u8; 2048];
        let read = tokio::time::timeout(Duration::from_secs(20), client.read(&mut chunk)).await;
        match read {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                reads += 1;
                received.extend_from_slice(&chunk[..n]);
            }
            _ => break,
        }
    }
    drop(client);
    hooks.shutdown_helper(helper_port).await;
    let upstream_request = upstream_server.await.unwrap();

    let text = String::from_utf8_lossy(&received).to_string();
    assert!(text.contains("200 OK"), "应返回 200 SSE 响应：{text}");
    assert!(
        text.contains("response.custom_tool_call_input.delta"),
        "客户端必须收到 custom input delta：{text}"
    );
    assert!(text.contains("response.custom_tool_call_input.done"));
    assert!(text.contains("\"input\":\"socket 流式 🧪\""));
    assert!(
        !text.contains("response.function_call_arguments"),
        "受管调用的 wire 事件不得泄漏给客户端"
    );
    assert!(
        reads >= 2,
        "SSE 必须分多次读入（reads={reads}），客户端边读边收"
    );

    // 上游收到的是 function 包装。
    let wire_tools = upstream_request["tools"].as_array().unwrap();
    assert_eq!(wire_tools[0]["type"], "function");
    assert_eq!(
        wire_tools[0]["parameters"]["properties"]["input"]["type"],
        "string"
    );
}

// ===========================================================================
// 受控 live A/B（默认 #[ignore]；CI 保持 mock）。显式提供隔离设置后手工运行：
//   cargo test -p codex-plus-core --test protocol_proxy live_ab_ -- --ignored --nocapture
// 环境变量：
//   CODEXPLUS_AB_SETTINGS   隔离 settings 模板路径（含唯一候选与凭据引用）
//   CODEXPLUS_AB_OUTPUT     脱敏报告输出目录
//   CODEXPLUS_AB_MODELS     逗号分隔的最终模型 ID（默认 coding-glm-5.3-flash）
//   CODEXPLUS_AB_TRIALS     每模型每组配对试验数（默认 3：1 非流式 + 2 流式）
//   CODEXPLUS_AB_MAX_REQUESTS 总请求预算上限（默认 32，触顶即停）
// 固定同一上游入口、同一凭据引用、同一模型 ID，只改变 customToolsAsFunctions。
// ===========================================================================

struct AbRecord {
    run_id: String,
    model: String,
    arm: &'static str,
    custom_tools_as_functions: bool,
    stream: bool,
    stage: &'static str,
    http_status: Option<u16>,
    upstream_call_type: Option<String>,
    client_call_type: Option<String>,
    input_exact_match: Option<bool>,
    call_id: String,
    tool_executed: bool,
    result_nonce: Option<String>,
    nonce_returned: Option<bool>,
    latency_ms: u128,
    usage: Option<serde_json::Value>,
    result: &'static str,
    note: String,
}

impl AbRecord {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "runId": self.run_id,
            "model": self.model,
            "arm": self.arm,
            "customToolsAsFunctions": self.custom_tools_as_functions,
            "stream": self.stream,
            "stage": self.stage,
            "httpStatus": self.http_status,
            "upstreamCallType": self.upstream_call_type,
            "clientCallType": self.client_call_type,
            "inputExactMatch": self.input_exact_match,
            "callId": self.call_id,
            "toolExecuted": self.tool_executed,
            "resultNonce": self.result_nonce,
            "resultNonceReturned": self.nonce_returned,
            "latencyMs": self.latency_ms,
            "usage": self.usage,
            "result": self.result,
            "note": self.note,
        })
    }
}

fn ab_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn ab_parse_sse_calls(
    text: &str,
) -> (
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
) {
    // 返回（最后一个 custom_tool_call done item、function_call done item、usage）
    let mut custom_item = None;
    let mut function_item = None;
    let mut usage = None;
    for frame in text.split("\n\n") {
        let Some(data_line) = frame.lines().find_map(|l| l.strip_prefix("data: ")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data_line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("response.completed") {
            usage = value.pointer("/response/usage").cloned();
        }
        if value.get("type").and_then(serde_json::Value::as_str)
            == Some("response.output_item.done")
        {
            let item = value
                .get("item")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("custom_tool_call") => custom_item = Some(item),
                Some("function_call") => function_item = Some(item),
                _ => {}
            }
        }
    }
    (custom_item, function_item, usage)
}

fn ab_json_calls(
    body: &serde_json::Value,
) -> (
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
) {
    let mut custom_item = None;
    let mut function_item = None;
    if let Some(items) = body.get("output").and_then(serde_json::Value::as_array) {
        for item in items {
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("custom_tool_call") => custom_item = Some(item.clone()),
                Some("function_call") => function_item = Some(item.clone()),
                _ => {}
            }
        }
    }
    (custom_item, function_item, body.get("usage").cloned())
}

#[tokio::test]
#[ignore = "live A/B：消耗真实上游额度，需显式提供 CODEXPLUS_AB_SETTINGS"]
async fn live_ab_custom_tools_as_functions_via_production_proxy() {
    let Some(settings_template) = ab_env("CODEXPLUS_AB_SETTINGS") else {
        panic!("缺少 CODEXPLUS_AB_SETTINGS（隔离设置模板路径）");
    };
    let output_dir =
        ab_env("CODEXPLUS_AB_OUTPUT").unwrap_or_else(|| ".review-local/ab/results".to_string());
    std::fs::create_dir_all(&output_dir).unwrap();
    let models: Vec<String> = ab_env("CODEXPLUS_AB_MODELS")
        .unwrap_or_else(|| "coding-glm-5.3-flash".to_string())
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    let trials: usize = ab_env("CODEXPLUS_AB_TRIALS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .clamp(1, 3);
    let max_requests: usize = ab_env("CODEXPLUS_AB_MAX_REQUESTS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);

    let mut template: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_template).unwrap()).unwrap();
    // base_url 是 skip_serializing 字段，必须直接在 JSON 上补齐，
    // 否则结构体往返会把 baseUrl 丢掉导致上游地址为空。
    if let Some(profiles) = template["relayProfiles"].as_array_mut() {
        for profile in profiles {
            profile["baseUrl"] = profile["upstreamBaseUrl"].clone();
        }
    }
    let vectors = [
        "echo round-trip-ok".to_string(),
        "line1\nline2 中文 🧪 保留原样".to_string(),
        "C:\\temp\\a b.txt \"引号\"".to_string(),
    ];

    let mut records: Vec<AbRecord> = Vec::new();
    let mut budget = 0usize;
    let run_id = format!(
        "ab-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    'models: for (model_index, model) in models.iter().enumerate() {
        let trial_count = if model_index == 0 { trials } else { 1 };
        for trial in 0..trial_count {
            let stream = trial != 0; // 1 非流式 + 其余流式
            let vector = &vectors[trial % vectors.len()];
            // AB / BA 交替，减少时间顺序偏差。
            let arms: &[(&str, bool)] = if trial % 2 == 0 {
                &[("A", false), ("B", true)]
            } else {
                &[("B", true), ("A", false)]
            };

            let mut nonce = String::new();
            let mut round1_items: Vec<serde_json::Value> = Vec::new();
            let mut replay_done = true;

            for (arm_name, flag) in arms {
                if budget + 2 > max_requests {
                    records.push(AbRecord {
                        run_id: run_id.clone(),
                        model: model.clone(),
                        arm: arm_name,
                        custom_tools_as_functions: *flag,
                        stream,
                        stage: "budget",
                        http_status: None,
                        upstream_call_type: None,
                        client_call_type: None,
                        input_exact_match: None,
                        call_id: String::new(),
                        tool_executed: false,
                        result_nonce: None,
                        nonce_returned: None,
                        latency_ms: 0,
                        usage: None,
                        result: "BLOCKED",
                        note: format!("触达请求预算上限 {max_requests}"),
                    });
                    break 'models;
                }

                // Round 1：首次请求。直接在 JSON Value 上切换开关，避免
                // 结构体序列化丢掉 skip_serializing 字段。
                let mut arm_settings = template.clone();
                arm_settings["relayProfiles"][0]["customToolsAsFunctions"] = json!(flag);
                arm_settings["activeRelayId"] = arm_settings["relayProfiles"][0]["id"].clone();
                let arm_file = std::env::temp_dir().join(format!(
                    "codexplus-ab-{}-{}-{}.json",
                    run_id, model_index, arm_name
                ));
                std::fs::write(&arm_file, serde_json::to_vec_pretty(&arm_settings).unwrap())
                    .unwrap();
                let _guard = SettingsPathGuard::set(arm_file.clone());

                let request = json!({
                    "model": model,
                    "stream": stream,
                    "tool_choice": "auto",
                    "max_output_tokens": 4096,
                    "tools": [
                        {
                            "type": "custom", "name": "review_echo",
                            "description": "Echo exact input; this is a safe transport test tool, not a shell.",
                            "format": { "type": "text" }
                        },
                        {
                            "type": "function", "name": "review_wait",
                            "description": "A normal function control tool. Always returns ok.",
                            "parameters": {
                                "type": "object",
                                "properties": { "token": { "type": "string" } },
                                "required": ["token"],
                                "additionalProperties": false
                            },
                            "strict": false
                        }
                    ],
                    "input": [
                        {
                            "role": "user",
                            "content": format!(
                                "This is a transport compatibility test. Call review_echo exactly once using this exact input string:\n{vector}\nDo not answer with a tool list or imitate a tool result in plain text."
                            )
                        }
                    ]
                });

                budget += 1;
                let started = std::time::Instant::now();
                let response = tokio::time::timeout(
                    Duration::from_secs(90),
                    codex_plus_core::protocol_proxy::handle_responses_proxy_request(
                        &request.to_string(),
                    ),
                )
                .await;
                let latency = started.elapsed().as_millis();
                let response = match response {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        let message = error.to_string();
                        let result = if message.contains("401")
                            || message.contains("403")
                            || message.contains("402")
                        {
                            "BLOCKED"
                        } else {
                            "FAIL"
                        };
                        records.push(AbRecord {
                            run_id: run_id.clone(),
                            model: model.clone(),
                            arm: arm_name,
                            custom_tools_as_functions: *flag,
                            stream,
                            stage: "round1",
                            http_status: None,
                            upstream_call_type: None,
                            client_call_type: None,
                            input_exact_match: None,
                            call_id: String::new(),
                            tool_executed: false,
                            result_nonce: None,
                            nonce_returned: None,
                            latency_ms: latency,
                            usage: None,
                            result,
                            note: format!("round1 error: {message}"),
                        });
                        replay_done = false;
                        continue;
                    }
                    Err(_) => {
                        records.push(AbRecord {
                            run_id: run_id.clone(),
                            model: model.clone(),
                            arm: arm_name,
                            custom_tools_as_functions: *flag,
                            stream,
                            stage: "round1",
                            http_status: None,
                            upstream_call_type: None,
                            client_call_type: None,
                            input_exact_match: None,
                            call_id: String::new(),
                            tool_executed: false,
                            result_nonce: None,
                            nonce_returned: None,
                            latency_ms: latency,
                            usage: None,
                            result: "FAIL",
                            note: "round1 超时（90s）".to_string(),
                        });
                        replay_done = false;
                        continue;
                    }
                };
                if !response.status.starts_with("2") {
                    records.push(AbRecord {
                        run_id: run_id.clone(),
                        model: model.clone(),
                        arm: arm_name,
                        custom_tools_as_functions: *flag,
                        stream,
                        stage: "round1",
                        http_status: None,
                        upstream_call_type: None,
                        client_call_type: None,
                        input_exact_match: None,
                        call_id: String::new(),
                        tool_executed: false,
                        result_nonce: None,
                        nonce_returned: None,
                        latency_ms: latency,
                        usage: None,
                        result: "FAIL",
                        note: format!("round1 http {}", response.status),
                    });
                    replay_done = false;
                    continue;
                }

                let (call_item, function_item, usage) = if stream {
                    let text = String::from_utf8_lossy(&response.body);
                    let (custom, function, usage) = ab_parse_sse_calls(&text);
                    (custom, function, usage)
                } else {
                    let body: serde_json::Value =
                        serde_json::from_slice(&response.body).unwrap_or(serde_json::Value::Null);
                    ab_json_calls(&body)
                };

                let Some(item) = call_item.clone().or(function_item.clone()) else {
                    records.push(AbRecord {
                        run_id: run_id.clone(),
                        model: model.clone(),
                        arm: arm_name,
                        custom_tools_as_functions: *flag,
                        stream,
                        stage: "round1",
                        http_status: Some(200),
                        upstream_call_type: None,
                        client_call_type: None,
                        input_exact_match: None,
                        call_id: String::new(),
                        tool_executed: false,
                        result_nonce: None,
                        nonce_returned: None,
                        latency_ms: latency,
                        usage,
                        result: "FAIL",
                        note: "上游没有产生任何工具调用（可能只回了文本）".to_string(),
                    });
                    replay_done = false;
                    continue;
                };

                let call_type = item
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let call_id = item
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let input = item
                    .get("input")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let exact = call_type == "custom_tool_call" && input == vector.as_str();

                // synthetic 执行器：只做字符串验收，不触 shell。
                nonce = format!(
                    "nonce-{:08x}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .subsec_nanos()
                );
                let result_payload = json!({
                    "runId": run_id,
                    "inputSha256": format!("{:x}", {
                        use std::hash::{{Hash, Hasher}};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        input.hash(&mut hasher);
                        hasher.finish()
                    }),
                    "resultNonce": nonce,
                });

                records.push(AbRecord {
                    run_id: run_id.clone(),
                    model: model.clone(),
                    arm: arm_name,
                    custom_tools_as_functions: *flag,
                    stream,
                    stage: "round1",
                    http_status: Some(200),
                    upstream_call_type: Some(call_type.clone()),
                    client_call_type: Some(call_type.clone()),
                    input_exact_match: Some(exact),
                    call_id: call_id.clone(),
                    tool_executed: true,
                    result_nonce: Some(nonce.clone()),
                    nonce_returned: None,
                    latency_ms: latency,
                    usage,
                    result: if exact { "PASS" } else { "FAIL" },
                    note: if exact {
                        String::new()
                    } else {
                        format!(
                            "input 不匹配（期待 {vector} 字符数 {}，实际 {}）",
                            vector.chars().count(),
                            input.chars().count()
                        )
                    },
                });

                round1_items = request["input"].as_array().cloned().unwrap_or_default();
                round1_items.push(if call_type == "custom_tool_call" {
                    json!({
                        "type": "custom_tool_call", "id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                        "call_id": call_id, "name": item.get("name").cloned().unwrap_or(serde_json::Value::Null),
                        "input": input, "status": "completed"
                    })
                } else {
                    json!({
                        "type": "function_call", "id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                        "call_id": call_id, "name": item.get("name").cloned().unwrap_or(serde_json::Value::Null),
                        "arguments": item.get("arguments").cloned().unwrap_or(serde_json::Value::Null),
                        "status": "completed"
                    })
                });
                round1_items.push(json!({
                    "type": "custom_tool_call_output", "call_id": call_id,
                    "output": result_payload.to_string()
                }));

                if budget >= max_requests {
                    replay_done = false;
                    continue;
                }

                // Round 2：工具结果回放（固定非流式， canonical 历史）。
                let replay = json!({
                    "model": model,
                    "stream": false,
                    "tool_choice": "auto",
                    "max_output_tokens": 4096,
                    "tools": request["tools"].clone(),
                    "input": {
                        "array": round1_items,
                    }
                });
                let mut replay_input = replay.clone();
                replay_input["input"] = json!(round1_items.clone());
                replay_input["input"].as_array_mut().unwrap().push(json!({
                    "role": "user",
                    "content": "The tool result is available now. Reply with exactly the resultNonce value and nothing else."
                }));

                budget += 1;
                let started = std::time::Instant::now();
                let replay_response = tokio::time::timeout(
                    Duration::from_secs(90),
                    codex_plus_core::protocol_proxy::handle_responses_proxy_request(
                        &replay_input.to_string(),
                    ),
                )
                .await;
                let latency = started.elapsed().as_millis();
                let nonce_returned = match replay_response {
                    Ok(Ok(response)) => {
                        let body: serde_json::Value = serde_json::from_slice(&response.body)
                            .unwrap_or(serde_json::Value::Null);
                        let text = body["output"]
                            .as_array()
                            .unwrap_or(&Vec::new())
                            .iter()
                            .filter_map(|item| {
                                item.pointer("/content/0/text")
                                    .and_then(serde_json::Value::as_str)
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        text.contains(&nonce)
                    }
                    _ => false,
                };
                records.push(AbRecord {
                    run_id: run_id.clone(),
                    model: model.clone(),
                    arm: arm_name,
                    custom_tools_as_functions: *flag,
                    stream: false,
                    stage: "round2_replay",
                    http_status: Some(200),
                    upstream_call_type: None,
                    client_call_type: None,
                    input_exact_match: None,
                    call_id,
                    tool_executed: true,
                    result_nonce: Some(nonce.clone()),
                    nonce_returned: Some(nonce_returned),
                    latency_ms: latency,
                    usage: None,
                    result: if nonce_returned { "PASS" } else { "FAIL" },
                    note: if nonce_returned {
                        String::new()
                    } else {
                        "模型未能消费工具结果中的 resultNonce".to_string()
                    },
                });
            }
            let _ = replay_done;
        }
    }

    // 输出 JSONL 与汇总。
    let jsonl_path = std::env::temp_dir().join(format!("codexplus-ab-{run_id}.jsonl"));
    let mut jsonl = String::new();
    let mut summary = json!({
        "runId": run_id,
        "budgetUsed": budget,
        "budgetCap": max_requests,
        "trialsPerModel": trials,
        "models": models,
        "records": []
    });
    for record in &records {
        jsonl.push_str(&record.to_json().to_string());
        jsonl.push('\n');
    }
    summary["records"] = json!(records.iter().map(|r| r.to_json()).collect::<Vec<_>>());
    std::fs::write(&jsonl_path, jsonl).unwrap();
    let summary_path = std::path::Path::new(&output_dir).join("ab-summary.json");
    std::fs::write(&summary_path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
    println!("AB summary -> {}", summary_path.display());
    println!("AB jsonl -> {}", jsonl_path.display());
    for record in &records {
        println!(
            "[AB] {} model={} arm={} stream={} stage={} result={} note={}",
            record.run_id,
            record.model,
            record.arm,
            record.stream,
            record.stage,
            record.result,
            record.note
        );
    }
    // 测试本身不因业务失败而失败：证据以报告为准（§14.9 允许多种结论）。
}
