//! Custom-as-Function 双向适配器（`customToolsAsFunctions`，供应商级、默认关闭）。
//!
//! 请求侧把 custom 工具声明/历史/tool_choice 包装成「单 `input` 字符串参数」的
//! function；响应侧只对**本适配器包装过的 wire 名**做精确逆变换，并按严格规则
//! 校验 `{"input": <string>}`：未知字段、重复键、缺字段、非字符串都明确失败，
//! 绝不猜测命令、绝不二次包装普通 function。
//!
//! 设计约定（与修复方案一致）：
//! - 每个请求候选持有自己的 [`CustomToolAdapterPlan`]，随
//!   `UpstreamProxyResponse` 返回；没有全局可变状态。
//! - 关闭时全部入口走字节透传 fast path，不解析任何事件。
//! - ID 策略：`call_id` 逐字符保留；客户端 item id 沿用仓库既有约定
//!   （`fc_`/`item_` 前缀换 `ctc_`，其余原样），同一调用的所有事件一致。

use std::collections::BTreeMap;

use serde::de::Visitor;
use serde::Deserializer;
use serde_json::Value;

/// 每个调用的 arguments 缓冲上限（字节）。
pub const MAX_ARGUMENTS_BYTES: usize = 8 * 1024 * 1024;
/// 单个响应内所有 pending 调用的合计缓冲上限（字节）。
pub const MAX_TOTAL_BUFFERED_BYTES: usize = 32 * 1024 * 1024;
/// 单个响应内并发的 pending 调用数上限。
pub const MAX_PENDING_CALLS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterErrorCode {
    InvalidArguments,
    ToolIdentityConflict,
    UnsupportedField,
    UnsupportedFormat,
    StateReferenceUnsupported,
    StreamIncomplete,
    BufferLimit,
    PolicyConflict,
}

impl AdapterErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArguments => "CUSTOM_ADAPTER_INVALID_ARGUMENTS",
            Self::ToolIdentityConflict => "CUSTOM_ADAPTER_TOOL_IDENTITY_CONFLICT",
            Self::UnsupportedField => "CUSTOM_ADAPTER_UNSUPPORTED_FIELD",
            Self::UnsupportedFormat => "CUSTOM_ADAPTER_UNSUPPORTED_FORMAT",
            Self::StateReferenceUnsupported => "CUSTOM_ADAPTER_STATE_REFERENCE_UNSUPPORTED",
            Self::StreamIncomplete => "CUSTOM_ADAPTER_STREAM_INCOMPLETE",
            Self::BufferLimit => "CUSTOM_ADAPTER_BUFFER_LIMIT",
            Self::PolicyConflict => "CUSTOM_ADAPTER_POLICY_CONFLICT",
        }
    }
}

/// 类型化适配错误。`Display` 以错误码开头，便于日志与客户端错误体引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub code: AdapterErrorCode,
    pub message: String,
}

impl AdapterError {
    pub fn new(code: AdapterErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AdapterError {}

/// 客户端视角的工具身份：原始工具名 + 所属 namespace（顶层为空串）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientToolIdentity {
    pub client_name: String,
    pub namespace: String,
}

/// 请求级适配计划：本候选实际发出的 function 包装与其客户端身份的精确映射。
/// 响应侧只信任这里登记过的 wire 名。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CustomToolAdapterPlan {
    wire_tools: BTreeMap<String, ClientToolIdentity>,
    /// wire 名 → 包装前的客户端视图 custom 声明（用于响应 tools/tool_choice 回显还原）。
    original_declarations: BTreeMap<String, Value>,
}

impl CustomToolAdapterPlan {
    pub fn is_empty(&self) -> bool {
        self.wire_tools.is_empty()
    }

    pub fn identity(&self, wire_name: &str) -> Option<&ClientToolIdentity> {
        self.wire_tools.get(wire_name)
    }

    pub fn original_declaration(&self, wire_name: &str) -> Option<&Value> {
        self.original_declarations.get(wire_name)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&String, &ClientToolIdentity)> {
        self.wire_tools.iter()
    }

    fn insert(&mut self, wire_name: String, identity: ClientToolIdentity, original: Value) {
        self.wire_tools.insert(wire_name.clone(), identity);
        self.original_declarations.insert(wire_name, original);
    }

    /// 按客户端身份（含可选 namespace 限定）反查 wire 名；同名多义时返回冲突错误。
    fn resolve_client_reference(
        &self,
        client_name: &str,
        namespace: Option<&str>,
    ) -> Result<Option<&String>, AdapterError> {
        let matches: Vec<&String> = self
            .wire_tools
            .iter()
            .filter(|(wire_name, identity)| {
                identity.client_name == client_name
                    && match namespace {
                        Some(namespace) if !namespace.is_empty() => {
                            identity.namespace == namespace
                        }
                        // 未限定 namespace 时，顶层 custom 才是无歧义引用；
                        // 只有 namespace 版本同名时也接受（客户端常见形状）。
                        _ => {
                            identity.namespace.is_empty()
                                || self
                                    .wire_tools
                                    .iter()
                                    .filter(|(_, other)| other.client_name == client_name)
                                    .count()
                                    == 1
                        }
                    }
                    && !(identity.namespace.is_empty()
                        && namespace.map(str::is_empty).unwrap_or(false)
                        && wire_name.as_str() != client_name)
            })
            .map(|(wire_name, _)| wire_name)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0])),
            _ => Err(AdapterError::new(
                AdapterErrorCode::ToolIdentityConflict,
                format!("tool_choice 的 custom 引用「{client_name}」命中多个同名工具，已拒绝歧义派发"),
            )),
        }
    }
}

/// 请求是否依赖服务端保存的调用状态（本适配器不支持迁移）。
pub fn has_unsupported_state_reference(body: &Value) -> bool {
    if body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return true;
    }
    if body.get("conversation").is_some_and(|value| !value.is_null()) {
        return true;
    }
    body.get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("item_reference"))
        })
}

/// Responses wire 请求编码：包装 custom 声明/历史/tool_choice。
///
/// 必须在 namespace 扁平化与 custom_tool_call ID 规范化之后调用；`namespace_tools`
/// 是扁平化器返回的权威身份映射（wire 名 → (namespace, 客户端原名)）。
pub fn encode_request(
    body: &mut Value,
    namespace_tools: &BTreeMap<String, (String, String)>,
) -> Result<CustomToolAdapterPlan, AdapterError> {
    let mut plan = CustomToolAdapterPlan::default();

    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools.iter_mut() {
            if tool.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            let Some(wire_name) = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
            else {
                return Err(AdapterError::new(
                    AdapterErrorCode::ToolIdentityConflict,
                    "custom 工具缺少可用名称，无法安全包装",
                ));
            };
            let (namespace, client_name) = match namespace_tools.get(&wire_name) {
                Some((namespace, original)) => (namespace.clone(), original.clone()),
                None => (String::new(), wire_name.clone()),
            };
            let wrapped = wrap_custom_tool_declaration(tool, &wire_name)?;
            // 客户端视图的原始声明：原名（+namespace 字段），供回显还原使用。
            let mut original = tool.clone();
            let original_object = original.as_object_mut().expect("custom 工具是对象");
            original_object.insert("name".to_string(), Value::String(client_name.clone()));
            if !namespace.is_empty() {
                original_object.insert("namespace".to_string(), Value::String(namespace.clone()));
            }
            *tool = wrapped;
            plan.insert(
                wire_name,
                ClientToolIdentity {
                    client_name,
                    namespace,
                },
                original,
            );
        }
    }

    convert_history_items(body)?;
    convert_tool_choice(body, &plan)?;

    Ok(plan)
}

/// 生成 function 包装声明。字段保留策略：
/// - description / format 语义并入包装描述（原 grammar 约束退化为文字指导）；
/// - 未知字段、不支持的 format 类型明确报错，不静默丢弃也不假定上游可接受。
fn wrap_custom_tool_declaration(tool: &Value, wire_name: &str) -> Result<Value, AdapterError> {
    for key in tool
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if !matches!(key.as_str(), "type" | "name" | "description" | "format") {
            return Err(AdapterError::new(
                AdapterErrorCode::UnsupportedField,
                format!("custom 工具「{wire_name}」携带暂不支持包装的字段「{key}」，已拒绝发送"),
            ));
        }
    }

    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    let mut grammar_note = String::new();
    match tool.get("format") {
        None | Some(Value::Null) => {}
        Some(format) => match format.get("type").and_then(Value::as_str) {
            Some("text") | Some("") => {}
            Some("grammar") => {
                let syntax = format.get("syntax").and_then(Value::as_str).unwrap_or("");
                grammar_note = format!(
                    "\n\nThe original custom tool declared a {syntax} grammar for its input; \
                     follow that grammar when composing the input text. The proxy cannot enforce \
                     it server-side anymore, so produce the exact text the tool expects."
                );
            }
            other => {
                return Err(AdapterError::new(
                    AdapterErrorCode::UnsupportedFormat,
                    format!(
                        "custom 工具「{wire_name}」的 format 类型「{}」不受本适配器支持",
                        other.unwrap_or("<missing>")
                    ),
                ));
            }
        },
    }

    let mut wrapper_description = if description.is_empty() {
        "Freeform custom tool wrapped as a function by codex-plus. Supply a JSON object with one \
         input string. The input value is the original tool text, not an additional JSON encoding \
         of that text."
            .to_string()
    } else {
        format!(
            "{description}\n\nSupply a JSON object with one input string. The input value is the \
             original tool text, not an additional JSON encoding of that text."
        )
    };
    wrapper_description.push_str(&grammar_note);

    Ok(serde_json::json!({
        "type": "function",
        "name": wire_name,
        "description": wrapper_description,
        "parameters": {
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Exact original custom-tool input, preserving whitespace and escapes."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        },
        "strict": false
    }))
}

/// 历史 custom 调用/结果 → function 调用/结果。`call_id` 与既有 item id 原样保留
/// （ID 规范化已在更早的管线步骤完成），只有类型、name 与 input/arguments 变化。
fn convert_history_items(body: &mut Value) -> Result<(), AdapterError> {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (index, item) in items.iter_mut().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("custom_tool_call") => {
                let Some(name) = item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                else {
                    return Err(AdapterError::new(
                        AdapterErrorCode::ToolIdentityConflict,
                        format!("input[{index}] 的 custom_tool_call 缺少工具名，无法回放"),
                    ));
                };
                let Value::String(input_text) = item.get("input").unwrap_or(&Value::Null) else {
                    return Err(AdapterError::new(
                        AdapterErrorCode::InvalidArguments,
                        format!(
                            "input[{index}]（{name}）的 custom_tool_call.input 缺失或不是字符串，拒绝回放"
                        ),
                    ));
                };
                let arguments = serde_json::to_string(&serde_json::json!({ "input": input_text }))
                    .unwrap_or_default();
                let object = item.as_object_mut().expect("custom_tool_call 必须是对象");
                object.insert("type".to_string(), Value::String("function_call".into()));
                object.insert("name".to_string(), Value::String(name));
                object.insert("arguments".to_string(), Value::String(arguments));
                object.remove("input");
            }
            Some("custom_tool_call_output") => {
                let object = item.as_object_mut().expect("custom_tool_call_output 必须是对象");
                object.insert(
                    "type".to_string(),
                    Value::String("function_call_output".into()),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// tool_choice 转换：仅对登记在计划中的 custom 引用改为 function 引用；
/// `auto`/`required`/`none` 与普通 function 引用保持不变。
fn convert_tool_choice(body: &mut Value, plan: &CustomToolAdapterPlan) -> Result<(), AdapterError> {
    let Some(tool_choice) = body.get_mut("tool_choice") else {
        return Ok(());
    };
    convert_tool_choice_value(tool_choice, plan)
}

fn convert_tool_choice_value(
    value: &mut Value,
    plan: &CustomToolAdapterPlan,
) -> Result<(), AdapterError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    match object.get("type").and_then(Value::as_str) {
        Some("custom") => {
            let Some(name) = object.get("name").and_then(Value::as_str) else {
                return Err(AdapterError::new(
                    AdapterErrorCode::ToolIdentityConflict,
                    "tool_choice 指向 custom 工具但缺少名称",
                ));
            };
            let namespace = object
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(wire_name) = plan
                .resolve_client_reference(name, (!namespace.is_empty()).then_some(namespace))?
                .cloned()
            else {
                return Err(AdapterError::new(
                    AdapterErrorCode::ToolIdentityConflict,
                    format!("tool_choice 引用的 custom 工具「{name}」不在本次工具集合中"),
                ));
            };
            object.insert("type".to_string(), Value::String("function".into()));
            object.insert("name".to_string(), Value::String(wire_name));
            object.remove("namespace");
            Ok(())
        }
        Some("allowed_tools") => {
            if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
                for entry in tools.iter_mut() {
                    if entry.get("type").and_then(Value::as_str) == Some("custom") {
                        convert_tool_choice_value(entry, plan)?;
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 严格解码 function_call.arguments：必须恰好是一个对象，含且仅含字符串 `input`。
/// 未知字段、重复键、尾部垃圾、非对象 JSON 都失败；不 trim、不剥围栏、不做转义猜测。
pub fn decode_wrapped_arguments(arguments: &str) -> Result<String, AdapterError> {
    let mut decoder = StrictArgumentsDecoder::default();
    let mut de = serde_json::Deserializer::from_str(arguments);
    let result = de.deserialize_any(StrictArgumentsVisitor {
        state: &mut decoder,
    });
    let result = result.and_then(|_| de.end());
    result.map_err(|error| {
        AdapterError::new(
            AdapterErrorCode::InvalidArguments,
            format!("function 参数不是合法的 {{\"input\": string}} 包装：{error}"),
        )
    })?;
    decoder.input.ok_or_else(|| {
        AdapterError::new(
            AdapterErrorCode::InvalidArguments,
            "function 参数缺少 input 字符串字段".to_string(),
        )
    })
}

#[derive(Debug, Default)]
struct StrictArgumentsDecoder {
    input: Option<String>,
    seen_input: bool,
}

struct StrictArgumentsVisitor<'a> {
    state: &'a mut StrictArgumentsDecoder,
}

fn arguments_error<E: serde::de::Error>(detail: &str) -> E {
    E::custom(format!(
        "expected a JSON object with exactly one string field `input`: {detail}"
    ))
}

impl<'de> Visitor<'de> for StrictArgumentsVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON object with exactly one string field `input`")
    }

    fn visit_bool<E>(self, _v: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(arguments_error("value is a boolean"))
    }

    fn visit_i64<E>(self, _v: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(arguments_error("value is a number"))
    }

    fn visit_u64<E>(self, _v: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(arguments_error("value is a number"))
    }

    fn visit_f64<E>(self, _v: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(arguments_error("value is a number"))
    }

    fn visit_str<E>(self, _v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(arguments_error("value is a plain string, not an object"))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(arguments_error("value is null"))
    }

    fn visit_seq<A>(self, _seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        Err(arguments_error("value is an array"))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while let Some(key) = map.next_key::<String>()? {
            if key == "input" {
                if self.state.seen_input {
                    return Err(arguments_error("duplicate field `input`"));
                }
                self.state.seen_input = true;
                let value = map
                    .next_value::<String>()
                    .map_err(|_| arguments_error("field `input` must be a string"))?;
                self.state.input = Some(value);
            } else {
                return Err(arguments_error(&format!("unexpected field `{key}`")));
            }
        }
        Ok(())
    }
}

/// 从 function_call item 构造客户端 custom_tool_call item 的公共逻辑。
/// `wire_name` 必须精确命中计划；失败一律返回类型化错误。
fn client_custom_tool_call_item(
    item: &Value,
    plan: &CustomToolAdapterPlan,
) -> Result<Value, AdapterError> {
    let wire_name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let Some(identity) = plan.identity(wire_name) else {
        return Err(AdapterError::new(
            AdapterErrorCode::ToolIdentityConflict,
            format!("响应中的 function_call「{wire_name}」不在本请求的包装计划内，拒绝猜测"),
        ));
    };
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AdapterError::new(
                AdapterErrorCode::ToolIdentityConflict,
                format!("function_call「{wire_name}」缺少 call_id，无法配对工具结果"),
            )
        })?;
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AdapterError::new(
                AdapterErrorCode::InvalidArguments,
                format!("function_call「{wire_name}」缺少 arguments 字符串"),
            )
        })?;
    let input = decode_wrapped_arguments(arguments)?;

    let mut converted = serde_json::Map::new();
    converted.insert("type".to_string(), Value::String("custom_tool_call".into()));
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .map(|id| client_custom_item_id(id, call_id));
    if let Some(item_id) = item_id {
        converted.insert("id".to_string(), Value::String(item_id));
    }
    converted.insert("call_id".to_string(), Value::String(call_id.to_string()));
    converted.insert(
        "name".to_string(),
        Value::String(identity.client_name.clone()),
    );
    converted.insert("input".to_string(), Value::String(input));
    if let Some(status) = item.get("status").and_then(Value::as_str) {
        converted.insert("status".to_string(), Value::String(status.to_string()));
    } else {
        converted.insert("status".to_string(), Value::String("completed".into()));
    }
    if !identity.namespace.is_empty() {
        converted.insert(
            "namespace".to_string(),
            Value::String(identity.namespace.clone()),
        );
    }
    Ok(Value::Object(converted))
}

/// 客户端 custom item id 规则：沿用仓库既有约定，`fc_`/`item_` 前缀换成 `ctc_`，
/// 其余（包括已是 `ctc_` 或原生 `ct_`）保持原样，保证不叠加前缀。
fn client_custom_item_id(upstream_id: &str, call_id: &str) -> String {
    if let Some(suffix) = upstream_id
        .strip_prefix("fc_")
        .or_else(|| upstream_id.strip_prefix("item_"))
    {
        return format!("ctc_{suffix}");
    }
    if !upstream_id.is_empty() {
        return upstream_id.to_string();
    }
    format!("ctc_{call_id}")
}

/// 非流式 JSON 响应还原：只作用于已识别的协议形状（根级 item、`item` 字段、
/// `output`、`response.output`），并把响应回显的 tools/tool_choice 还原为
/// 客户端原始视图。metadata、示例 JSON 等业务数据不做任何扫描。
pub fn restore_custom_tool_calls_json(
    value: &mut Value,
    plan: &CustomToolAdapterPlan,
) -> Result<(), AdapterError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    if let Some(item) = object.get_mut("item") {
        if is_managed_call(item, plan) {
            *item = client_custom_tool_call_item(item, plan)?;
        }
    }
    for container in ["output", "response"] {
        match object.get_mut(container) {
            Some(Value::Array(items)) => {
                for item in items.iter_mut() {
                    if is_managed_call(item, plan) {
                        *item = client_custom_tool_call_item(item, plan)?;
                    }
                }
            }
            Some(Value::Object(response)) => {
                if let Some(Value::Array(items)) = response.get_mut("output") {
                    for item in items.iter_mut() {
                        if is_managed_call(item, plan) {
                            *item = client_custom_tool_call_item(item, plan)?;
                        }
                    }
                }
                // §10.4：响应回显的 tools/tool_choice 按原始声明还原，保持客户端
                // 视角自洽（声明是 custom，调用也是 custom）。
                restore_echoed_tools(response, plan);
            }
            _ => {}
        }
    }
    restore_echoed_tools(object, plan);
    Ok(())
}

fn is_managed_call(item: &Value, plan: &CustomToolAdapterPlan) -> bool {
    item.get("type").and_then(Value::as_str) == Some("function_call")
        && item
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| plan.identity(name).is_some())
}

fn restore_echoed_tools(object: &mut serde_json::Map<String, Value>, plan: &CustomToolAdapterPlan) {
    if let Some(Value::Array(tools)) = object.get_mut("tools") {
        for tool in tools.iter_mut() {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                if let Some(original) = plan.original_declaration(name).cloned() {
                    *tool = original;
                }
            }
        }
    }
    if let Some(choice) = object.get_mut("tool_choice") {
        let managed = choice.get("type").and_then(Value::as_str) == Some("function")
            && choice
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| plan.identity(name).is_some());
        if managed {
            if let Some(name) = choice.get("name").and_then(Value::as_str) {
                if let Some(identity) = plan.identity(name) {
                    let mut restored = serde_json::Map::new();
                    restored.insert("type".to_string(), Value::String("custom".into()));
                    restored.insert(
                        "name".to_string(),
                        Value::String(identity.client_name.clone()),
                    );
                    if !identity.namespace.is_empty() {
                        restored.insert(
                            "namespace".to_string(),
                            Value::String(identity.namespace.clone()),
                        );
                    }
                    *choice = Value::Object(restored);
                }
            }
        }
    }
}

/// Chat 上游请求用的适配计划：只登记「namespace 容器内的 custom 子工具」，
/// 顶层 custom 走既有 Chat 转换路径，不属于本计划。
pub fn chat_plan_for_namespace_customs(original_tools: Option<&Value>) -> CustomToolAdapterPlan {
    let mut plan = CustomToolAdapterPlan::default();
    let Some(tools) = original_tools.and_then(Value::as_array) else {
        return plan;
    };
    for tool in tools {
        if tool.get("type").and_then(Value::as_str) != Some("namespace") {
            continue;
        }
        collect_namespace_custom_plan(tool, "", &mut plan);
    }
    plan
}

fn collect_namespace_custom_plan(
    namespace_tool: &Value,
    parent_namespace: &str,
    plan: &mut CustomToolAdapterPlan,
) {
    let namespace = crate::protocol_proxy::flatten_namespace_tool_name(
        parent_namespace,
        namespace_tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let Some(children) = namespace_tool
        .get("tools")
        .and_then(Value::as_array)
        .or_else(|| namespace_tool.get("children").and_then(Value::as_array))
    else {
        return;
    };
    for child in children {
        match child.get("type").and_then(Value::as_str) {
            Some("namespace") => collect_namespace_custom_plan(child, &namespace, plan),
            Some("custom") => {
                if let Some(name) =
                    child.get("name").and_then(Value::as_str).filter(|v| !v.is_empty())
                {
                    let flat = crate::protocol_proxy::flatten_namespace_tool_name(&namespace, name);
                    let mut original = child.clone();
                    if let Some(object) = original.as_object_mut() {
                        object.insert("name".to_string(), Value::String(name.to_string()));
                    }
                    plan.insert(
                        flat,
                        ClientToolIdentity {
                            client_name: name.to_string(),
                            namespace: namespace.clone(),
                        },
                        original,
                    );
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// SSE：按调用缓冲、完整校验后交付（V1 不做增量 JSON 字符串解析）。
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct PendingCall {
    item_id: String,
    call_id: String,
    wire_name: String,
    arguments: String,
    /// `.done` 事件带来的权威全量 arguments（若上游提供）。
    done_arguments: Option<String>,
    delivered_input: Option<String>,
}


#[derive(Debug)]
struct StreamFailure {
    code: AdapterErrorCode,
    message: String,
}

/// Responses wire 实时流的 custom 适配重写器。
/// 只缓冲命中包装计划的调用事件；普通文本、reasoning、普通 function 与心跳
/// 按原路径流动。计划为空时退化为逐字节透传 fast path。
#[derive(Debug, Default)]
pub struct CustomToolSseRewriter {
    plan: CustomToolAdapterPlan,
    buffer: Vec<u8>,
    /// 按 output_index 索引的 pending 调用。
    pending: BTreeMap<u32, PendingCall>,
    /// item_id → output_index（item_id 已知后建立）。
    item_index: BTreeMap<String, u32>,
    sequence: u64,
    total_buffered: usize,
    failure: Option<StreamFailure>,
}

impl CustomToolSseRewriter {
    pub fn new(plan: CustomToolAdapterPlan) -> Self {
        Self {
            plan,
            ..Default::default()
        }
    }

    pub fn plan(&self) -> &CustomToolAdapterPlan {
        &self.plan
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.plan.is_empty() {
            return bytes.to_vec();
        }
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        while self.failure.is_none() {
            let Some(frame) = take_complete_frame(&mut self.buffer) else {
                break;
            };
            self.process_frame(&frame, &mut output);
        }
        if self.failure.is_some() && !self.buffer.is_empty() {
            // 失败后不再缓冲/转换，剩余字节原样透传（通常是上游收尾）。
            output.extend_from_slice(&self.buffer);
            self.buffer.clear();
        }
        output
    }

    /// 收尾：完整但未终止的末帧照常处理；半帧丢弃并报告截断。
    /// 有未交付的 pending 调用时先发出协议 error 事件，不伪造成功。
    /// 返回 (输出, 是否丢弃了不完整半帧)。
    pub fn finish_with_truncation(&mut self) -> (Vec<u8>, bool) {
        let mut output = Vec::new();
        let mut truncated = false;
        if self.plan.is_empty() {
            if !self.buffer.is_empty() {
                output.extend_from_slice(&self.buffer);
                self.buffer.clear();
            }
            return (output, false);
        }
        let buffer = std::mem::take(&mut self.buffer);
        if !buffer.is_empty() {
            let text = String::from_utf8_lossy(&buffer);
            let parsed = parse_sse_frame(&buffer);
            if parsed.data_json().is_some() {
                // 完整 JSON 但缺空行终止：仍按帧处理，但标记截断。
                self.process_frame(&buffer, &mut output);
                truncated = true;
            } else if !text.trim().is_empty() {
                truncated = true;
            }
        }
        if self.failure.is_none() && !self.pending.is_empty() {
            let dropped = self.pending.len();
            self.fail(
                AdapterErrorCode::StreamIncomplete,
                format!("上游流在 {dropped} 个工具调用交付前结束，已丢弃待执行输入"),
            );
        }
        self.pending.clear();
        self.item_index.clear();
        if let Some(failure) = &self.failure {
            push_error_event(&mut output, failure, &mut self.sequence);
        }
        (output, truncated)
    }

    fn fail(&mut self, code: AdapterErrorCode, message: impl Into<String>) {
        if self.failure.is_none() {
            self.failure = Some(StreamFailure {
                code,
                message: message.into(),
            });
        }
    }

    fn process_frame(&mut self, frame: &[u8], output: &mut Vec<u8>) {
        let parsed = parse_sse_frame(frame);
        let Some(data_json) = parsed.data_json() else {
            // 注释、[DONE] 等非 JSON 帧：原样透传（未交付检查在 finish）。
            output.extend_from_slice(frame);
            return;
        };
        if parsed.event.as_deref() == Some("error")
            || data_json.get("type").and_then(Value::as_str) == Some("error")
        {
            // 上游已报告错误：转发并停止交付任何 pending。
            self.fail(
                AdapterErrorCode::StreamIncomplete,
                "上游流返回 error 事件".to_string(),
            );
            self.emit_frame(output, parsed.event.as_deref(), &data_json);
            return;
        }

        match data_json.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.output_item.added" => {
                self.handle_output_item_added(&data_json, &parsed, output)
            }
            "response.function_call_arguments.delta" => {
                self.handle_arguments_delta(&data_json, &parsed, output)
            }
            "response.function_call_arguments.done" => {
                self.handle_arguments_done(&data_json, &parsed, output)
            }
            "response.output_item.done" => {
                self.handle_output_item_done(&data_json, &parsed, output)
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                self.handle_response_terminal(&data_json, &parsed, output)
            }
            _ => self.emit_frame(output, parsed.event.as_deref(), &data_json),
        }
    }

    fn handle_output_item_added(&mut self, data: &Value, parsed: &SseFrame, output: &mut Vec<u8>) {
        let Some(item) = data.get("item") else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        }
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        if self.plan.identity(name).is_none() {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        }
        let output_index = data
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32;
        if self.pending.contains_key(&output_index) {
            self.fail(
                AdapterErrorCode::ToolIdentityConflict,
                format!("output_index {output_index} 出现重复的 function_call added"),
            );
            return;
        }
        if self.pending.len() >= MAX_PENDING_CALLS {
            self.fail(
                AdapterErrorCode::BufferLimit,
                format!("单个响应的并发工具调用超过上限 {MAX_PENDING_CALLS}"),
            );
            return;
        }
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !item_id.is_empty() {
            self.item_index.insert(item_id.clone(), output_index);
        }
        self.pending.insert(
            output_index,
            PendingCall {
                item_id,
                call_id: item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                wire_name: name.to_string(),
                arguments: String::new(),
                done_arguments: None,
                delivered_input: None,
            },
        );
        // added 事件被吸收，待完整校验后按 custom 事件组交付。
    }

    fn resolve_pending_index(&self, data: &Value) -> Option<u32> {
        if let Some(item_id) = data.get("item_id").and_then(Value::as_str) {
            if let Some(output_index) = self.item_index.get(item_id) {
                return Some(*output_index);
            }
        }
        if let Some(output_index) = data.get("output_index").and_then(Value::as_u64) {
            let output_index = output_index as u32;
            if self.pending.contains_key(&output_index) {
                return Some(output_index);
            }
        }
        None
    }

    fn handle_arguments_delta(&mut self, data: &Value, parsed: &SseFrame, output: &mut Vec<u8>) {
        let Some(delta) = data.get("delta").and_then(Value::as_str) else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        let Some(index) = self.resolve_pending_index(data) else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        let Some(pending) = self.pending.get_mut(&index) else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        if pending.delivered_input.is_some() {
            self.fail(
                AdapterErrorCode::ToolIdentityConflict,
                "同一调用在交付后仍收到 arguments delta",
            );
            return;
        }
        pending.arguments.push_str(delta);
        self.total_buffered += delta.len();
        if pending.arguments.len() > MAX_ARGUMENTS_BYTES
            || self.total_buffered > MAX_TOTAL_BUFFERED_BYTES
        {
            self.fail(
                AdapterErrorCode::BufferLimit,
                format!(
                    "arguments 缓冲超过上限（单调用 {MAX_ARGUMENTS_BYTES} / 合计 {MAX_TOTAL_BUFFERED_BYTES} 字节）"
                ),
            );
        }
    }

    fn handle_arguments_done(&mut self, data: &Value, parsed: &SseFrame, output: &mut Vec<u8>) {
        let Some(arguments) = data.get("arguments").and_then(Value::as_str) else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        let Some(index) = self.resolve_pending_index(data) else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        let Some(pending) = self.pending.get_mut(&index) else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        if pending.delivered_input.is_some() {
            self.fail(
                AdapterErrorCode::ToolIdentityConflict,
                "同一调用在交付后仍收到 arguments done",
            );
            return;
        }
        // 一致性在解码值层面核对：上游允许对同一 JSON 使用不同转义形式。
        if !pending.arguments.is_empty() && !arguments.is_empty() {
            match (
                decode_wrapped_arguments(&pending.arguments),
                decode_wrapped_arguments(arguments),
            ) {
                (Ok(buffered_input), Ok(done_input)) if buffered_input != done_input => {
                    self.fail(
                        AdapterErrorCode::InvalidArguments,
                        "arguments delta 与 done 解码后的内容不一致，拒绝猜测",
                    );
                    return;
                }
                // delta 累计不完整或转义形式不同时，以 done 的权威全量为准。
                _ => {}
            }
        }
        pending.done_arguments = Some(arguments.to_string());
    }

    fn handle_output_item_done(&mut self, data: &Value, parsed: &SseFrame, output: &mut Vec<u8>) {
        let Some(item) = data.get("item") else {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            self.emit_frame(output, parsed.event.as_deref(), data);
            return;
        }
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let output_index = data
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32;
        let managed_by_name = self.plan.identity(name).is_some();
        let known_index = self
            .resolve_pending_index(data)
            .filter(|index| self.pending.contains_key(index));
        match known_index {
            None if managed_by_name => {
                // completed-only 或 added 被上游省略：直接从 done item 校验交付。
                self.deliver_from_item(item, output_index, output);
            }
            None => {
                self.emit_frame(output, parsed.event.as_deref(), data);
            }
            Some(index) => {
                let (pending_item_id, pending_call_id, pending_wire_name, delivered_input, done_arguments, buffered) = {
                    let pending = self.pending.get(&index).expect("index checked");
                    (
                        pending.item_id.clone(),
                        pending.call_id.clone(),
                        pending.wire_name.clone(),
                        pending.delivered_input.clone(),
                        pending.done_arguments.clone(),
                        pending.arguments.clone(),
                    )
                };
                if let Some(delivered) = delivered_input {
                    // 重复的 done（S09）：不重复派发；内容不一致则显式失败。
                    let final_arguments =
                        item.get("arguments").and_then(Value::as_str).unwrap_or("");
                    if !final_arguments.is_empty()
                        && decode_wrapped_arguments(final_arguments)
                            .is_ok_and(|input| input != delivered)
                    {
                        self.fail(
                            AdapterErrorCode::InvalidArguments,
                            "重复的 output_item.done 与已交付内容不一致",
                        );
                    }
                    return;
                }
                if name != pending_wire_name {
                    self.fail(
                        AdapterErrorCode::ToolIdentityConflict,
                        format!(
                            "output_index {index} 的调用身份在 added 与 done 之间变化（{pending_wire_name} → {name}）"
                        ),
                    );
                    return;
                }
                if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                    if !pending_item_id.is_empty()
                        && !item_id.is_empty()
                        && item_id != pending_item_id
                    {
                        self.fail(
                            AdapterErrorCode::ToolIdentityConflict,
                            format!("同一调用的 item id 发生变化：{pending_item_id} → {item_id}"),
                        );
                        return;
                    }
                }
                // §11.3：item 状态为 incomplete/failed 时禁止交付可执行输入。
                let item_status = item.get("status").and_then(Value::as_str).unwrap_or("");
                if matches!(item_status, "incomplete" | "failed") {
                    self.pending.remove(&index);
                    self.item_index.retain(|_, value| *value != index);
                    return;
                }
                let final_arguments =
                    item.get("arguments").and_then(Value::as_str).map(str::to_string);
                // delta / done / done-item 三方一致性核对（§11.2）：有契约的权威
                // 表示是 done 事件全量，其次 done item 内的全量，最后是累计 delta。
                let authoritative = done_arguments
                    .filter(|value| !value.is_empty())
                    .or(final_arguments.filter(|value| !value.is_empty()))
                    .unwrap_or(buffered);
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&pending_call_id)
                    .to_string();
                self.deliver_call(
                    index,
                    &pending_item_id,
                    &call_id,
                    name,
                    &authoritative,
                    item.get("status").and_then(Value::as_str),
                    output,
                );
            }
        }
    }

    fn deliver_from_item(&mut self, item: &Value, output_index: u32, output: &mut Vec<u8>) {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if call_id.is_empty() {
            self.fail(
                AdapterErrorCode::ToolIdentityConflict,
                "function_call 缺少 call_id，无法配对工具结果",
            );
            return;
        }
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.deliver_call(
            output_index,
            &item_id,
            call_id,
            name,
            arguments,
            item.get("status").and_then(Value::as_str),
            output,
        );
    }

    /// 校验完整参数并交付 custom 事件组（added → input.delta → input.done → done）。
    #[allow(clippy::too_many_arguments)]
    fn deliver_call(
        &mut self,
        output_index: u32,
        item_id: &str,
        call_id: &str,
        wire_name: &str,
        arguments: &str,
        status: Option<&str>,
        output: &mut Vec<u8>,
    ) {
        let input = match decode_wrapped_arguments(arguments) {
            Ok(input) => input,
            Err(error) => {
                self.fail(error.code, error.message);
                return;
            }
        };
        let Some(identity) = self.plan.identity(wire_name).cloned() else {
            self.fail(
                AdapterErrorCode::ToolIdentityConflict,
                format!("交付时找不到「{wire_name}」的包装计划"),
            );
            return;
        };
        let client_item_id = client_custom_item_id(item_id, call_id);
        let item_status = status.unwrap_or("in_progress");

        let mut added_item = serde_json::Map::new();
        added_item.insert("id".to_string(), Value::String(client_item_id.clone()));
        added_item.insert("type".to_string(), Value::String("custom_tool_call".into()));
        added_item.insert("status".to_string(), Value::String(item_status.to_string()));
        added_item.insert("call_id".to_string(), Value::String(call_id.to_string()));
        added_item.insert(
            "name".to_string(),
            Value::String(identity.client_name.clone()),
        );
        added_item.insert("input".to_string(), Value::String(String::new()));
        if !identity.namespace.is_empty() {
            added_item.insert(
                "namespace".to_string(),
                Value::String(identity.namespace.clone()),
            );
        }
        self.emit_frame(
            output,
            Some("response.output_item.added"),
            &serde_json::json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": Value::Object(added_item)
            }),
        );

        if !input.is_empty() {
            self.emit_frame(
                output,
                Some("response.custom_tool_call_input.delta"),
                &serde_json::json!({
                    "type": "response.custom_tool_call_input.delta",
                    "item_id": client_item_id,
                    "call_id": call_id,
                    "output_index": output_index,
                    "delta": input
                }),
            );
        }
        self.emit_frame(
            output,
            Some("response.custom_tool_call_input.done"),
            &serde_json::json!({
                "type": "response.custom_tool_call_input.done",
                "item_id": client_item_id,
                "call_id": call_id,
                "output_index": output_index,
                "input": input
            }),
        );

        let done_status = status.unwrap_or("completed");
        let done_item = serde_json::json!({
            "id": client_item_id,
            "type": "custom_tool_call",
            "status": done_status,
            "call_id": call_id,
            "name": identity.client_name,
            "input": input
        });
        self.emit_frame(
            output,
            Some("response.output_item.done"),
            &serde_json::json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": done_item
            }),
        );

        if let Some(pending) = self.pending.get_mut(&output_index) {
            pending.delivered_input = Some(input);
        }
    }

    fn handle_response_terminal(&mut self, data: &Value, parsed: &SseFrame, output: &mut Vec<u8>) {
        let failed_terminal =
            data.get("type").and_then(Value::as_str) != Some("response.completed");
        let mut converted = data.clone();
        if let Some(response) = converted.get_mut("response") {
            if let Some(items) = response.get_mut("output").and_then(Value::as_array_mut) {
                for slot in items.iter_mut() {
                    let item = slot.clone();
                    if !is_managed_call(&item, &self.plan) {
                        continue;
                    }
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let already_delivered = self.pending.values().any(|pending| {
                        pending.wire_name == name && pending.delivered_input.is_some()
                    });
                    if failed_terminal && !already_delivered {
                        // 失败/不完整终态：不交付可执行输入，转为不可执行的
                        // custom 视图并保留状态，绝不冒充成功。
                        *slot = undelivered_terminal_view(&item, &self.plan);
                    } else {
                        // 已交付过：核对一致性；未交付过（completed-only）：现在校验。
                        match self.terminal_item_view(&item, already_delivered) {
                            Ok(restored) => *slot = restored,
                            Err(error) => {
                                self.fail(error.code, error.message);
                                return;
                            }
                        }
                    }
                }
            }
            if let Some(object) = response.as_object_mut() {
                // 终态帧内回显的 tools/tool_choice 一并还原（§10.4）。
                restore_echoed_tools(object, &self.plan);
            }
        }
        // 终态之后本响应的调用生命周期结束。
        self.pending.clear();
        self.item_index.clear();
        self.emit_frame(output, parsed.event.as_deref(), &converted);
    }

    /// 已交付：核对终态 item 与已交付输入一致，并转换为 custom 视图。
    /// 未交付（completed-only 精简流）：现在校验并交付，转换为 custom 视图。
    fn terminal_item_view(
        &mut self,
        item: &Value,
        already_delivered: bool,
    ) -> Result<Value, AdapterError> {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let identity = self.plan.identity(name).ok_or_else(|| {
            AdapterError::new(
                AdapterErrorCode::ToolIdentityConflict,
                format!("终态 item「{name}」不在包装计划内"),
            )
        })?;
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AdapterError::new(
                    AdapterErrorCode::InvalidArguments,
                    format!("终态 function_call「{name}」缺少 arguments"),
                )
            })?;
        let input = decode_wrapped_arguments(arguments)?;
        if already_delivered {
            let delivered = self
                .pending
                .values()
                .find(|pending| pending.wire_name == name)
                .and_then(|pending| pending.delivered_input.clone());
            if let Some(delivered) = delivered {
                if delivered != input {
                    return Err(AdapterError::new(
                        AdapterErrorCode::InvalidArguments,
                        format!("终态 output 中「{name}」的输入与已交付内容不一致，拒绝猜测"),
                    ));
                }
            }
        }
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AdapterError::new(
                    AdapterErrorCode::ToolIdentityConflict,
                    format!("终态 function_call「{name}」缺少 call_id"),
                )
            })?
            .to_string();
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .map(|id| client_custom_item_id(id, &call_id))
            .unwrap_or_else(|| format!("ctc_{call_id}"));
        let mut restored = serde_json::Map::new();
        restored.insert("id".to_string(), Value::String(item_id));
        restored.insert("type".to_string(), Value::String("custom_tool_call".into()));
        restored.insert(
            "status".to_string(),
            Value::String(
                item.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                    .to_string(),
            ),
        );
        restored.insert("call_id".to_string(), Value::String(call_id));
        restored.insert(
            "name".to_string(),
            Value::String(identity.client_name.clone()),
        );
        restored.insert("input".to_string(), Value::String(input));
        if !identity.namespace.is_empty() {
            restored.insert(
                "namespace".to_string(),
                Value::String(identity.namespace.clone()),
            );
        }
        Ok(Value::Object(restored))
    }

    fn emit_frame(&mut self, output: &mut Vec<u8>, event: Option<&str>, data: &Value) {
        let mut data = data.clone();
        if let Some(object) = data.as_object_mut() {
            object.insert("sequence_number".to_string(), Value::from(self.sequence));
            self.sequence += 1;
        }
        let event = event
            .map(str::to_string)
            .or_else(|| data.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        output.extend_from_slice(b"event: ");
        output.extend_from_slice(event.as_bytes());
        output.extend_from_slice(b"\ndata: ");
        output.extend_from_slice(serde_json::to_string(&data).unwrap_or_default().as_bytes());
        output.extend_from_slice(b"\n\n");
    }
}

/// 失败/不完整终态里未交付的受管调用：转成不可执行的 custom 视图
/// （输入为空、保留原始状态），绝不冒充可执行的成功调用。
fn undelivered_terminal_view(item: &Value, plan: &CustomToolAdapterPlan) -> Value {
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let identity = plan.identity(name);
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .map(|id| client_custom_item_id(id, call_id))
        .unwrap_or_else(|| format!("ctc_{call_id}"));
    serde_json::json!({
        "id": item_id,
        "type": "custom_tool_call",
        "status": item.get("status").and_then(Value::as_str).unwrap_or("incomplete"),
        "call_id": call_id,
        "name": identity.map(|identity| identity.client_name.clone()).unwrap_or_else(|| name.to_string()),
        "input": ""
    })
}

/// SSE 分帧：取最早的 `\n\n` / `\r\n\r\n` 边界；完整 JSON 但缺终止行时按
/// 单帧处理（返回 complete=false）。返回 (边界位置, 是否有终止行)。
fn find_frame_boundary(buffer: &[u8]) -> Option<(usize, bool)> {
    let mut best: Option<usize> = None;
    for (index, window) in buffer.windows(2).enumerate() {
        if window == b"\n\n" {
            best = Some(index + 2);
            break;
        }
    }
    for (index, window) in buffer.windows(4).enumerate() {
        if window == b"\r\n\r\n" {
            let end = index + 4;
            if best.is_none_or(|existing| end < existing) {
                best = Some(end);
            }
            break;
        }
    }
    if let Some(end) = best {
        return Some((end, true));
    }
    // 上游偶发不写空行终止：data 已是完整 JSON 时按帧收下，但不算正常终止。
    let parsed = parse_sse_frame(buffer);
    if parsed.data_json().is_some() {
        return Some((buffer.len(), false));
    }
    None
}

fn take_complete_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (end, _complete) = find_frame_boundary(buffer)?;
    Some(buffer.drain(..end).collect())
}

#[derive(Debug, Default)]
struct SseFrame {
    event: Option<String>,
    data: String,
}

impl SseFrame {
    fn data_json(&self) -> Option<Value> {
        let trimmed = self.data.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str(trimmed).ok()
    }
}

fn parse_sse_frame(frame: &[u8]) -> SseFrame {
    let text = String::from_utf8_lossy(frame);
    let mut result = SseFrame::default();
    let mut data_lines: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(event) = line.strip_prefix("event:") {
            result.event = Some(event.trim().to_string());
        } else if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    result.data = data_lines.join("\n");
    result
}

fn push_error_event(output: &mut Vec<u8>, failure: &StreamFailure, sequence: &mut u64) {
    let payload = serde_json::json!({
        "type": "error",
        "code": failure.code.as_str(),
        "message": failure.message,
        "sequence_number": *sequence,
    });
    *sequence += 1;
    output.extend_from_slice(b"event: error\ndata: ");
    output.extend_from_slice(serde_json::to_string(&payload).unwrap_or_default().as_bytes());
    output.extend_from_slice(b"\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// §13.3 精确字符串向量：装箱 → 解码必须逐字符相等，不做任何规范化。
    #[test]
    fn exact_input_vectors_round_trip() {
        let vectors: Vec<String> = vec![
            String::new(),
            "  前后都有空格  ".to_string(),
            "line1\nline2".to_string(),
            "line1\r\nline2".to_string(),
            "C:\\测试目录\\a b.txt".to_string(),
            "单双引号：\"hello\" 'world'".to_string(),
            "emoji：🧪，中文：工具回放".to_string(),
            "{\"cmd\":\"already-json\"}".to_string(),
            "*** Begin Patch\n*** Update File: probe.txt\n@@\n-old\n+new\n*** End Patch".to_string(),
        ];
        for vector in vectors {
            let wrapped = serde_json::to_string(&json!({ "input": vector })).unwrap();
            let decoded = decode_wrapped_arguments(&wrapped).unwrap();
            assert_eq!(decoded, vector, "input 必须逐字符保留");
        }
    }

    #[test]
    fn invalid_arguments_fail_closed() {
        let cases = [
            "not json at all",
            "42",
            "\"plain string\"",
            "null",
            "[1,2,3]",
            "{}",
            "{\"input\":null}",
            "{\"input\":42}",
            "{\"input\":[\"a\"]}",
            "{\"input\":\"a\",\"extra\":1}",
            "{\"input\":\"a\",\"input\":\"b\"}",
            "{\"input\":\"a\"} trailing",
        ];
        for case in cases {
            let error = decode_wrapped_arguments(case).unwrap_err();
            assert_eq!(error.code, AdapterErrorCode::InvalidArguments, "case: {case}");
        }
        // 合法空串必须成功（不被 falsy 逻辑丢弃）。
        assert_eq!(decode_wrapped_arguments("{\"input\":\"\"}").unwrap(), "");
    }

    fn namespace_tools_fixture() -> BTreeMap<String, (String, String)> {
        BTreeMap::from([(
            "functions__review_echo".to_string(),
            ("functions".to_string(), "review_echo".to_string()),
        )])
    }

    #[test]
    fn encode_wraps_custom_declarations_and_history() {
        let mut body = serde_json::json!({
            "model": "m",
            "tools": [
                { "type": "custom", "name": "functions__review_echo", "description": "Echo tool.", "format": { "type": "text" } },
                { "type": "function", "name": "review_wait", "parameters": { "type": "object", "properties": { "token": { "type": "string" } } } }
            ],
            "input": [
                { "role": "user", "content": "go" },
                { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "functions__review_echo", "input": "第一行\n  第二行\\路径\"引号\"  " },
                { "type": "custom_tool_call_output", "call_id": "call_1", "output": "done" }
            ],
            "tool_choice": { "type": "custom", "namespace": "functions", "name": "review_echo" }
        });
        let plan = encode_request(&mut body, &namespace_tools_fixture()).unwrap();
        assert!(!plan.is_empty());

        let tools = body["tools"].as_array().unwrap();
        let echo = tools
            .iter()
            .find(|tool| tool["name"] == "functions__review_echo")
            .unwrap();
        assert_eq!(echo["type"], "function");
        assert_eq!(echo["strict"], false);
        assert_eq!(echo["parameters"]["properties"]["input"]["type"], "string");
        assert_eq!(echo["parameters"]["required"], json!(["input"]));
        assert_eq!(echo["parameters"]["additionalProperties"], false);
        // 普通 function 不受影响。
        let wait = tools.iter().find(|tool| tool["name"] == "review_wait").unwrap();
        assert_eq!(wait["type"], "function");
        assert!(wait.get("parameters").is_some());

        let items = body["input"].as_array().unwrap();
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "call_1");
        assert_eq!(items[1]["name"], "functions__review_echo");
        let arguments = items[1]["arguments"].as_str().unwrap();
        assert_eq!(decode_wrapped_arguments(arguments).unwrap(), "第一行\n  第二行\\路径\"引号\"  ");
        assert!(items[1].get("input").is_none());
        assert_eq!(items[2]["type"], "function_call_output");
        assert_eq!(items[2]["call_id"], "call_1");

        assert_eq!(body["tool_choice"], json!({ "type": "function", "name": "functions__review_echo" }));

        // 计划身份：客户端视角还原为原名 + namespace。
        let identity = plan.identity("functions__review_echo").unwrap();
        assert_eq!(identity.client_name, "review_echo");
        assert_eq!(identity.namespace, "functions");
    }

    #[test]
    fn encode_rejects_unknown_fields_and_bad_format() {
        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "weird", "defer_loading": true } ]
        });
        let error = encode_request(&mut body, &BTreeMap::new()).unwrap_err();
        assert_eq!(error.code, AdapterErrorCode::UnsupportedField);

        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "weird", "format": { "type": "regex" } } ]
        });
        let error = encode_request(&mut body, &BTreeMap::new()).unwrap_err();
        assert_eq!(error.code, AdapterErrorCode::UnsupportedFormat);
    }

    #[test]
    fn grammar_meta_is_preserved_as_description_note() {
        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "patchy", "format": { "type": "grammar", "syntax": "lark", "definition": "start: TEXT" } } ]
        });
        encode_request(&mut body, &BTreeMap::new()).unwrap();
        let description = body["tools"][0]["description"].as_str().unwrap();
        assert!(description.contains("lark"), "grammar 语法要进入包装描述");
    }

    #[test]
    fn json_restore_recovers_exact_input_and_removes_arguments() {
        let plan_json = serde_json::json!({
            "tools": [ { "type": "custom", "name": "review_echo", "description": "Echo tool." } ]
        });
        let mut body = plan_json.clone();
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut response = serde_json::json!({
            "id": "resp_1",
            "object": "response",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_abc",
                    "call_id": "call_9",
                    "name": "review_echo",
                    "arguments": "{\"input\":\"line1\\r\\nline2 \u{1f9ea}\"}",
                    "status": "completed"
                }
            ]
        });
        restore_custom_tool_calls_json(&mut response, &plan).unwrap();
        let item = &response["output"][0];
        assert_eq!(item["type"], "custom_tool_call");
        assert_eq!(item["name"], "review_echo");
        assert_eq!(item["id"], "ctc_abc");
        assert_eq!(item["call_id"], "call_9");
        assert_eq!(item["input"], "line1\r\nline2 🧪");
        assert!(item.get("arguments").is_none());
        assert_eq!(item["status"], "completed");
    }

    #[test]
    fn json_restore_skips_plain_functions_and_metadata() {
        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "review_echo" } ]
        });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut response = serde_json::json!({
            "output": [
                { "type": "function_call", "id": "fc_x", "call_id": "c1", "name": "review_wait", "arguments": "{\"input\":\"nope\"}" },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "{\"type\":\"function_call\",\"name\":\"review_echo\"}" }] }
            ]
        });
        restore_custom_tool_calls_json(&mut response, &plan).unwrap();
        assert_eq!(response["output"][0]["type"], "function_call", "普通 function 不误转");
        assert_eq!(
            response["output"][1]["content"][0]["text"],
            "{\"type\":\"function_call\",\"name\":\"review_echo\"}",
            "业务 JSON 不被扫描改写"
        );
    }

    #[test]
    fn json_restore_reverts_echoed_tools_and_tool_choice() {
        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "review_echo", "description": "Echo." } ],
            "tool_choice": { "type": "custom", "name": "review_echo" }
        });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut response = serde_json::json!({
            "response": {
                "output": [],
                "tools": [ { "type": "function", "name": "review_echo", "description": "wrapped" } ],
                "tool_choice": { "type": "function", "name": "review_echo" }
            }
        });
        restore_custom_tool_calls_json(&mut response, &plan).unwrap();
        let echoed = &response["response"];
        assert_eq!(echoed["tools"][0]["type"], "custom");
        assert_eq!(echoed["tools"][0]["description"], "Echo.");
        assert_eq!(echoed["tool_choice"], json!({ "type": "custom", "name": "review_echo" }));
    }

    #[test]
    fn unmanaged_function_call_in_response_passes_through() {
        let mut body = serde_json::json!({ "tools": [ { "type": "custom", "name": "review_echo" } ] });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut response = serde_json::json!({
            "output": [
                { "type": "function_call", "call_id": "c1", "name": "totally_other", "arguments": "{\"input\":\"x\"}" },
                { "type": "function_call", "call_id": "c2", "name": "review_wait", "arguments": "{\"token\":\"y\"}" }
            ]
        });
        restore_custom_tool_calls_json(&mut response, &plan).unwrap();
        // 只有计划内的 wire 名才还原；其他 function 原样保留（J02）。
        assert_eq!(response["output"][0]["type"], "function_call");
        assert_eq!(response["output"][0]["name"], "totally_other");
        assert_eq!(response["output"][1]["type"], "function_call");
        assert_eq!(response["output"][1]["name"], "review_wait");
        // 计划内的调用正常还原。
        response["output"][0]["name"] = json!("review_echo");
        restore_custom_tool_calls_json(&mut response, &plan).unwrap();
        assert_eq!(response["output"][0]["type"], "custom_tool_call");
    }

    fn sse_frame(event: &str, mut payload: Value) -> String {
        payload["type"] = json!(event);
        format!("event: {event}\ndata: {}\n\n", payload)
    }

    #[test]
    fn sse_buffers_and_delivers_custom_group_with_sequence_renumbering() {
        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "review_echo" }, { "type": "function", "name": "review_wait", "parameters": {} } ]
        });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut rewriter = CustomToolSseRewriter::new(plan);

        let mut upstream = String::new();
        upstream.push_str(&sse_frame(
            "response.created",
            json!({ "response": { "id": "resp" } }),
        ));
        upstream.push_str(&sse_frame(
            "response.output_item.added",
            json!({ "output_index": 0, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_a", "name": "review_echo", "arguments": "" } }),
        ));
        upstream.push_str(&sse_frame(
            "response.function_call_arguments.delta",
            json!({ "item_id": "fc_1", "output_index": 0, "delta": "{\"inp" }),
        ));
        upstream.push_str(&sse_frame(
            "response.function_call_arguments.delta",
            json!({ "item_id": "fc_1", "output_index": 0, "delta": "ut\":\"hi \\ud83e\\uddea\"}" }),
        ));
        upstream.push_str(&sse_frame(
            "response.function_call_arguments.done",
            json!({ "item_id": "fc_1", "output_index": 0, "arguments": "{\"input\":\"hi 🧪\"}" }),
        ));
        upstream.push_str(&sse_frame(
            "response.output_item.done",
            json!({ "output_index": 0, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_a", "name": "review_echo", "arguments": "{\"input\":\"hi 🧪\"}", "status": "completed" } }),
        ));
        upstream.push_str(&sse_frame(
            "response.completed",
            json!({ "response": { "id": "resp", "output": [ { "type": "function_call", "id": "fc_1", "call_id": "call_a", "name": "review_echo", "arguments": "{\"input\":\"hi 🧪\"}", "status": "completed" } ] } }),
        ));

        // 分片喂入（含 UTF-8/JSON escape 跨 chunk）。
        let bytes = upstream.as_bytes();
        let mut full = Vec::new();
        for chunk in bytes.chunks(7) {
            full.extend_from_slice(&rewriter.push_bytes(chunk));
        }
        let (tail, truncated) = rewriter.finish_with_truncation();
        full.extend_from_slice(&tail);
        assert!(!truncated);

        let text = String::from_utf8(full).unwrap();
        let events: Vec<(String, Value)> = text
            .split("\n\n")
            .filter(|frame| !frame.trim().is_empty())
            .map(|frame| {
                let mut lines = frame.lines();
                let event = lines
                    .next()
                    .unwrap()
                    .strip_prefix("event: ")
                    .unwrap()
                    .to_string();
                let data: Value = serde_json::from_str(
                    lines.next().unwrap().strip_prefix("data: ").unwrap(),
                )
                .unwrap();
                (event, data)
            })
            .collect();

        let names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "response.created",
                "response.output_item.added",
                "response.custom_tool_call_input.delta",
                "response.custom_tool_call_input.done",
                "response.output_item.done",
                "response.completed",
            ],
            "added/arguments delta 被吸收，custom 事件组一次性交付"
        );

        for (_, data) in &events {
            assert!(
                data.get("sequence_number").is_some(),
                "活跃适配流必须统一分配 sequence_number"
            );
        }
        let sequences: Vec<u64> = events
            .iter()
            .map(|(_, data)| data["sequence_number"].as_u64().unwrap())
            .collect();
        let mut sorted = sequences.clone();
        sorted.sort();
        assert_eq!(sequences, sorted, "sequence_number 必须单调递增");

        let added = &events[1].1["item"];
        assert_eq!(added["type"], "custom_tool_call");
        assert_eq!(added["id"], "ctc_1");
        assert_eq!(added["name"], "review_echo");
        assert_eq!(added["input"], "");
        let delta = events[2].1["delta"].as_str().unwrap();
        assert_eq!(delta, "hi 🧪");
        let done_item = &events[4].1["item"];
        assert_eq!(done_item["input"], "hi 🧪");
        assert_eq!(done_item["call_id"], "call_a", "call_id 逐字符保留");
        // 终态 output 也已转换。
        assert_eq!(events[5].1["response"]["output"][0]["type"], "custom_tool_call");
    }

    #[test]
    fn sse_passes_plain_functions_through_untouched() {
        let mut body = serde_json::json!({
            "tools": [ { "type": "custom", "name": "review_echo" } ]
        });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut rewriter = CustomToolSseRewriter::new(plan);
        let upstream = format!(
            "{}{}",
            sse_frame(
                "response.output_item.added",
                json!({ "output_index": 1, "item": { "id": "fc_2", "type": "function_call", "call_id": "call_b", "name": "review_wait", "arguments": "" } }),
            ),
            sse_frame(
                "response.output_item.done",
                json!({ "output_index": 1, "item": { "id": "fc_2", "type": "function_call", "call_id": "call_b", "name": "review_wait", "arguments": "{\"token\":\"x\"}", "status": "completed" } }),
            ),
        );
        let out = rewriter.push_bytes(upstream.as_bytes());
        let (tail, truncated) = rewriter.finish_with_truncation();
        assert!(!truncated);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"review_wait\""));
        assert!(
            !text.contains("custom_tool_call"),
            "普通 function 不该被转成 custom"
        );
        assert!(
            text.contains("\\\"token\\\":\\\"x\\\""),
            "普通 function 的 arguments 原样透传"
        );
        assert!(String::from_utf8_lossy(&tail).is_empty());
    }

    #[test]
    fn sse_inconsistent_delta_and_done_fails() {
        let mut body = serde_json::json!({ "tools": [ { "type": "custom", "name": "review_echo" } ] });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut rewriter = CustomToolSseRewriter::new(plan);
        let mut upstream = String::new();
        upstream.push_str(&sse_frame(
            "response.output_item.added",
            json!({ "output_index": 0, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_a", "name": "review_echo", "arguments": "" } }),
        ));
        upstream.push_str(&sse_frame(
            "response.function_call_arguments.delta",
            json!({ "item_id": "fc_1", "output_index": 0, "delta": "{\"input\":\"aaa\"}" }),
        ));
        upstream.push_str(&sse_frame(
            "response.function_call_arguments.done",
            json!({ "item_id": "fc_1", "output_index": 0, "arguments": "{\"input\":\"bbb\"}" }),
        ));
        let mut out = rewriter.push_bytes(upstream.as_bytes());
        let (tail, _truncated) = rewriter.finish_with_truncation();
        out.extend_from_slice(&tail);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"type\":\"error\""), "不一致必须显式失败");
        assert!(!text.contains("custom_tool_call_input"), "失败时不得交付输入");
    }

    #[test]
    fn sse_eof_with_pending_emits_error_not_success() {
        let mut body = serde_json::json!({ "tools": [ { "type": "custom", "name": "review_echo" } ] });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut rewriter = CustomToolSseRewriter::new(plan);
        let upstream = sse_frame(
            "response.output_item.added",
            json!({ "output_index": 0, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_a", "name": "review_echo", "arguments": "" } }),
        );
        rewriter.push_bytes(upstream.as_bytes());
        let (tail, truncated) = rewriter.finish_with_truncation();
        let text = String::from_utf8(tail).unwrap();
        assert!(text.contains("CUSTOM_ADAPTER_STREAM_INCOMPLETE"));
        assert!(!text.contains("custom_tool_call_input.done"));
        assert!(!truncated);
    }

    #[test]
    fn sse_half_frame_at_eof_is_reported_truncated() {
        let mut body = serde_json::json!({ "tools": [ { "type": "custom", "name": "review_echo" } ] });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut rewriter = CustomToolSseRewriter::new(plan);
        let out = rewriter.push_bytes(b"event: response.output_item.added\ndata: {\"partial\"");
        assert!(out.is_empty());
        let (tail, truncated) = rewriter.finish_with_truncation();
        // 半帧被丢弃并报告截断；added 未成功注册，因此没有待交付调用，
        // 也没有任何伪造的 custom 事件。
        assert!(truncated);
        assert!(!String::from_utf8_lossy(&tail).contains("custom_tool_call"));
    }

    #[test]
    fn sse_incomplete_terminal_never_delivers_input() {
        let mut body = serde_json::json!({ "tools": [ { "type": "custom", "name": "review_echo" } ] });
        let plan = encode_request(&mut body, &BTreeMap::new()).unwrap();
        let mut rewriter = CustomToolSseRewriter::new(plan);
        let mut upstream = String::new();
        upstream.push_str(&sse_frame(
            "response.output_item.added",
            json!({ "output_index": 0, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_a", "name": "review_echo", "arguments": "" } }),
        ));
        upstream.push_str(&sse_frame(
            "response.output_item.done",
            json!({ "output_index": 0, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_a", "name": "review_echo", "arguments": "{\"input\":\"rm -rf /\"}", "status": "incomplete" } }),
        ));
        upstream.push_str(&sse_frame(
            "response.incomplete",
            json!({ "response": { "id": "resp", "status": "incomplete", "output": [ { "type": "function_call", "id": "fc_1", "call_id": "call_a", "name": "review_echo", "arguments": "{\"input\":\"rm -rf /\"}", "status": "incomplete" } ] } }),
        ));
        let out = rewriter.push_bytes(upstream.as_bytes());
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("\"input\":\"rm -rf /\""),
            "incomplete 终态不得交付可执行输入"
        );
        let done_count = text.matches("response.custom_tool_call_input.done").count();
        assert_eq!(done_count, 0, "incomplete 终态不得产生 done 交付");
    }

    #[test]
    fn empty_plan_is_byte_fast_path() {
        let mut rewriter = CustomToolSseRewriter::default();
        let payload = sse_frame(
            "response.output_item.added",
            json!({ "output_index": 0, "item": { "type": "function_call", "name": "x" } }),
        );
        let out = rewriter.push_bytes(payload.as_bytes());
        assert_eq!(out, payload.as_bytes(), "空计划必须逐字节透传");
        let (tail, truncated) = rewriter.finish_with_truncation();
        assert!(tail.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn chat_plan_collects_only_namespace_customs() {
        let tools = serde_json::json!([
            { "type": "custom", "name": "top_level" },
            { "type": "namespace", "name": "functions", "tools": [
                { "type": "custom", "name": "review_echo" },
                { "type": "function", "name": "review_wait", "parameters": {} }
            ] }
        ]);
        let plan = chat_plan_for_namespace_customs(Some(&tools));
        assert!(plan.identity("top_level").is_none(), "顶层 custom 归既有 Chat 转换管");
        let identity = plan.identity("functions__review_echo").unwrap();
        assert_eq!(identity.client_name, "review_echo");
        assert_eq!(identity.namespace, "functions");
    }

    #[test]
    fn state_reference_detection() {
        assert!(has_unsupported_state_reference(
            &json!({ "previous_response_id": "resp_1" })
        ));
        assert!(has_unsupported_state_reference(
            &json!({ "conversation": { "id": "c1" } })
        ));
        assert!(has_unsupported_state_reference(&json!({
            "input": [ { "type": "item_reference", "id": "item_1" } ]
        })));
        assert!(!has_unsupported_state_reference(
            &json!({ "input": [ { "role": "user" } ] })
        ));
    }
}
