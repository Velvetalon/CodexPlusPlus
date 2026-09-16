use serde_json::{Value, json};

const NATIVE_NAMESPACE: &str = "collaboration";
const WIRE_NAMESPACE: &str = "codexpp_native_collaboration";

fn is_message_tool(name: &str) -> bool {
    matches!(name, "spawn_agent" | "send_message" | "followup_task")
}

pub(crate) fn interop_enabled(profile: &crate::settings::RelayProfile) -> bool {
    use crate::settings::NativeAgentInterop;
    match profile.native_agent_interop {
        NativeAgentInterop::On => true,
        NativeAgentInterop::Off => false,
        NativeAgentInterop::Auto => profile.protocol == crate::settings::RelayProtocol::Responses,
    }
}

pub(crate) fn prepare_request(body: &mut Value) -> bool {
    let mut changed = false;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        changed |= prepare_tools(tools);
    }
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                    changed |= prepare_tools(tools);
                }
            } else if item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("namespace").and_then(Value::as_str) == Some(NATIVE_NAMESPACE)
            {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                // Old encrypted history keeps its original backend identity.
                if !is_message_tool(name)
                    || item
                        .get("encrypted_function_args")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                {
                    item["namespace"] = json!(WIRE_NAMESPACE);
                    changed = true;
                }
            }
        }
    }
    changed
}

fn prepare_tools(tools: &mut [Value]) -> bool {
    let mut changed = false;
    for tool in tools {
        if tool.get("type").and_then(Value::as_str) != Some("namespace")
            || tool.get("name").and_then(Value::as_str) != Some(NATIVE_NAMESPACE)
        {
            continue;
        }
        // The official backend reserves collaboration's encrypted schema. A reversible
        // wire alias allows plaintext while Codex still dispatches its native tools.
        tool["name"] = json!(WIRE_NAMESPACE);
        if let Some(children) = tool.get_mut("tools").and_then(Value::as_array_mut) {
            for child in children {
                if is_message_tool(child.get("name").and_then(Value::as_str).unwrap_or("")) {
                    if let Some(message) = child
                        .pointer_mut("/parameters/properties/message")
                        .and_then(Value::as_object_mut)
                    {
                        message.insert("encrypted".to_string(), json!(false));
                    }
                }
            }
        }
        changed = true;
    }
    changed
}

pub(crate) fn prepare_glm_messages(body: &mut Value) {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in input {
        if item.get("type").and_then(Value::as_str) != Some("agent_message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        if !content.is_empty()
            && content
                .iter()
                .all(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
        {
            *item = json!({"type": "message", "role": "user", "content": content});
        }
    }
}

fn restore_call(item: &mut Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("function_call")
        || item.get("namespace").and_then(Value::as_str) != Some(WIRE_NAMESPACE)
    {
        return false;
    }
    item["namespace"] = json!(NATIVE_NAMESPACE);
    if is_message_tool(item.get("name").and_then(Value::as_str).unwrap_or(""))
        && item
            .get("encrypted_function_args")
            .is_none_or(Value::is_null)
    {
        // Codex uses [] (not an omitted field) to select DirectPlaintextMessage.
        item["encrypted_function_args"] = json!([]);
    }
    true
}

fn restore_response(value: &mut Value) -> bool {
    let mut changed = restore_call(value);
    if let Some(item) = value.get_mut("item") {
        changed |= restore_call(item);
    }
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        for item in output {
            changed |= restore_call(item);
        }
    }
    if let Some(response) = value.get_mut("response") {
        changed |= restore_response(response);
    }
    changed
}

pub(crate) fn restore_json(bytes: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return bytes.to_vec();
    };
    if restore_response(&mut value) {
        serde_json::to_vec(&value).expect("JSON value serialization")
    } else {
        bytes.to_vec()
    }
}

#[derive(Default)]
pub(crate) struct NativeAgentSseRewriter {
    buffer: Vec<u8>,
}

impl NativeAgentSseRewriter {
    pub(crate) fn push_bytes(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        loop {
            let lf = self
                .buffer
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| i + 2);
            let crlf = self
                .buffer
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| i + 4);
            let Some(end) = lf.into_iter().chain(crlf).min() else {
                break;
            };
            let frame: Vec<_> = self.buffer.drain(..end).collect();
            output.extend(rewrite_frame(&frame));
        }
        output
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffer)
    }
}

fn rewrite_frame(frame: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(frame) else {
        return frame.to_vec();
    };
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
        return frame.to_vec();
    };
    if !restore_response(&mut value) {
        return frame.to_vec();
    }
    let mut output = String::new();
    let mut wrote_data = false;
    for line in text.split_inclusive('\n') {
        if line.starts_with("data:") {
            if !wrote_data {
                output.push_str("data: ");
                output.push_str(&value.to_string());
                output.push_str(if line.ends_with("\r\n") { "\r\n" } else { "\n" });
                wrote_data = true;
            }
        } else {
            output.push_str(line);
        }
    }
    output.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_agent_interop_mode_overrides_automatic_detection() {
        use crate::settings::{NativeAgentInterop, RelayProfile, RelayProtocol};
        let mut profile = RelayProfile {
            base_url: "http://127.0.0.1:8788/v1".to_string(),
            protocol: RelayProtocol::Responses,
            ..Default::default()
        };
        assert!(interop_enabled(&profile));
        profile.native_agent_interop = NativeAgentInterop::On;
        assert!(interop_enabled(&profile));
        profile.native_agent_interop = NativeAgentInterop::Off;
        assert!(!interop_enabled(&profile));
    }

    #[test]
    fn native_tools_keep_every_definition_and_round_trip_with_plaintext_marker() {
        let namespace = json!({"type": "namespace", "name": "collaboration", "tools": [
            {"type": "function", "name": "spawn_agent", "parameters": {"properties": {
                "message": {"type": "string", "encrypted": true}, "model": {"type": "string"}
            }}},
            {"type": "function", "name": "wait_agent", "parameters": {"type": "object"}}
        ]});
        let extra = json!({"type": "future_tool", "keep": [1, 2]});
        let mut request =
            json!({"input": [{"type": "additional_tools", "tools": [namespace, extra]}]});
        assert!(prepare_request(&mut request));
        assert_eq!(request["input"][0]["tools"][0]["name"], WIRE_NAMESPACE);
        assert_eq!(request["input"][0]["tools"][1], extra);
        assert_eq!(
            request["input"][0]["tools"][0]["tools"][0]["parameters"]["properties"]["message"]["encrypted"],
            false
        );
        let arguments = r#"{"message":"run the task","model":"gpt-5.6-luna"}"#;
        let call = json!({"type": "function_call", "namespace": WIRE_NAMESPACE,
            "name": "spawn_agent", "arguments": arguments, "call_id": "call1"});
        let restored: Value = serde_json::from_slice(&restore_json(
            &serde_json::to_vec(&json!({"output": [call]})).unwrap(),
        ))
        .unwrap();
        assert_eq!(restored["output"][0]["namespace"], NATIVE_NAMESPACE);
        assert_eq!(restored["output"][0]["encrypted_function_args"], json!([]));
        assert_eq!(restored["output"][0]["arguments"], arguments);
        let mut replay = json!({"input": restored["output"]});
        assert!(prepare_request(&mut replay));
        assert_eq!(replay["input"][0]["namespace"], WIRE_NAMESPACE);
    }

    #[test]
    fn glm_keeps_task_text_and_leaves_opaque_old_messages_untouched() {
        let content =
            json!([{"type": "input_text", "text": "Message Type: NEW_TASK\nPayload:\nDo work"}]);
        let encrypted = json!({"type": "agent_message", "content": [
            {"type": "encrypted_content", "encrypted_content": "opaque"}
        ]});
        let mut body = json!({"input": [
            {"type": "agent_message", "author": "/root", "content": content}, encrypted
        ]});
        prepare_glm_messages(&mut body);
        assert_eq!(
            body["input"][0],
            json!({"type": "message", "role": "user", "content": content})
        );
        assert_eq!(body["input"][1], encrypted);
        let mut old_call = json!({"type": "function_call", "namespace": WIRE_NAMESPACE,
            "name": "send_message", "encrypted_function_args": ["message"]});
        restore_call(&mut old_call);
        assert_eq!(old_call["encrypted_function_args"], json!(["message"]));
        let mut history = json!({"input": [old_call]});
        let original = history.clone();
        assert!(!prepare_request(&mut history));
        assert_eq!(history, original);
    }

    #[test]
    fn fragmented_sse_restores_only_alias_calls_and_preserves_other_frames() {
        let unchanged = b": ping\r\n\r\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
        let event = json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "namespace": WIRE_NAMESPACE, "name": "followup_task",
            "arguments": "{\"message\":\"next task\"}"
        }});
        let frame = format!("id: 7\r\nevent: response.output_item.done\r\ndata: {event}\r\n\r\n");
        let mut input = unchanged.to_vec();
        input.extend(frame.as_bytes());
        input.extend(b"data: [DONE]\n\n");
        for chunk_size in 1..=17 {
            let mut rewriter = NativeAgentSseRewriter::default();
            let mut output = Vec::new();
            for chunk in input.chunks(chunk_size) {
                output.extend(rewriter.push_bytes(chunk));
            }
            output.extend(rewriter.finish());
            assert!(output.starts_with(unchanged));
            assert!(output.ends_with(b"data: [DONE]\n\n"));
            let text = String::from_utf8(output).unwrap();
            assert!(text.contains("\"namespace\":\"collaboration\""));
            assert!(text.contains("\"encrypted_function_args\":[]"));
            assert!(text.contains("id: 7\r\n"));
            assert!(!text.contains(WIRE_NAMESPACE));
        }
    }
}
