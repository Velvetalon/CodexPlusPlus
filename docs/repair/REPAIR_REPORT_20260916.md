# CodexPlusPlus 修复报告（2026-09-16）

## 范围

- 起始 HEAD：`4dbd055864ec9a9a7602be41e310b7c402ab65c4`（`fix/priority-fallback-web-search-429-20260829` 分支头部，即审查 HEAD）
- 结束 HEAD：`423546a`（分支 `fix/review-repair-20260916-234500`，共 4 个新提交）
- 工作分支/worktree：`E:\Code\CodexPlusPlus-repair-20260916-234500`（独立 worktree，原克隆 `E:\Code\CodexPlusPlus` 保持 main 不动）
- Codex/Node/Rust 版本：codex-cli 0.133.0；Node v20.11.1（系统）+ v22.18.0（便携版，仅用于前端 `.ts` 测试）；rustc/cargo 1.98.1 stable（本任务中经 rustup 安装）；npm 10.2.4
- 是否动过真实配置/数据/运行中进程：**否**。全部测试使用 tempfile/loopback mock；未触碰 `57321` 端口、真实 `.codex` 目录、运行中的 WorkBuddy/Codex 会话；未 push、未改版本号、未替换任何可执行文件。

## 结论

- **已确认并修复**：R01、R02、R03、R04、R05、R06、R07、R08（文案+结构透传边界）、R10、R12。
- **已核验、无需代码修改**：R09（有损行为均为显式 opt-in 策略，边界已记录）、R11（proxy/loopback/认证一致性由既有 relay_config / provider_switch_isolation / launcher 测试覆盖，未发现回退证据）。
- **剩余风险/未执行**：见下文「未验证项」与逐项附注。

## 问题逐项

| ID | 修复前证据 | 修复位置 | 修复后测试 | 状态 |
|----|------------|----------|------------|------|
| R01 | 冻结片段 + 生产路径 harness 红：resume 抛错后 `turn/start` 仍发出（turn=1）、目标上下文被缓存为成功、调用方无错误反馈 | `assets/inject/renderer-inject.js` `patchAppServerModelRequestClient`：`refreshCodexThreadModelBeforeTurn` 返回 `false` 时抛错阻断 turn；`turn/start` 成功结果不再写绑定缓存（只有 `thread/start`/`thread/resume` 算服务端确认）；resume 返回的 thread.model 与请求不一致时报 mismatch | `injection_script_blocks_turn_until_thread_binding_confirmed` F01/F02 绿；turn=0、调用方收到错误、失败后重试 resume（共 2 次） | ✅ 修复 |
| R02 | 生产路径 harness 红：bus dispatch resolve 即写缓存、发 `thread_model_context_refreshed`（F04 全红） | `dispatchMessageBusThreadModelRefresh` 重写为 ACK barrier：先注册按 requestId 的等待者再 dispatch；通过 `bus.subscribe("mcp-response")` 与 window `message` 捕获双通道等待真实回复；失败/超时/断连/过期阻断原始 turn 并发 `turn_blocked_until_thread_binding_confirmed` 诊断；`dispatchMessage` 返回值不作为业务 ACK；等待者/定时器/pending 在 settle 时全部清理，promise 永不 reject | F04（无回复不发 turn/不缓存/不发成功事件）、F05a（错误 ACK）、F05b（超时，pending 清理）、F06（异 id/异 hostId 回复不解锁）、F14（延迟 ACK）全绿 | ✅ 修复 |
| R03 | 源码确认 + 集成红测试：`prepare_request` 在 fallback 循环外对 `request_json` 原地执行、恢复标记取自首个候选（A01 红：B 继承 A 的 `encrypted:false`；A02 红：B 未执行自己的别名预处理） | `protocol_proxy.rs::open_responses_proxy_request_with_settings_and_user_agent`：循环内每候选 `request_json.clone()` 后独立执行 `prepare_request`；`UpstreamProxyResponse` 的 `native_agent_plaintext`/`namespace_tools` 来自产生响应的那个 attempt | A01（A on→429→B off：B 无别名/无明文标记）、A02（A off→429→B on：B 完整执行自己的预处理与恢复）、A04（两候选不同别名各按自身配置解析）、A08（原始请求体不被污染）全绿 | ✅ 修复 |
| R04 | 单元红测试 T01：顶层 `strict:false` 丢失（重组后为 None） | `flatten_responses_namespace_children`：从子工具完整 `clone` 开始，仅改 `type`/wire `name` 并按既有策略合并描述；嵌套 `function` 形状仅回填顶层缺失的 `parameters`/`strict`（顶层显式值优先）；`defer_loading`/`allowed_callers`/`async`/`output_schema`/未知字段全部保留 | `review_namespace_function_preserves_explicit_fields` 绿 | ✅ 修复 |
| R05 | 集成红测试 T04：顶层 `fs__read` 与 namespace `fs`+`read` 冲突时静默覆盖、请求照发上游 | 新增 `check_flattened_tool_name`：不同身份争用同一 wire name → 发送上游前返回带冲突身份的类型化错误；同身份不同定义 → 显式报错；同身份同定义 → 去重。冲突路径不发任何上游请求 | `namespace_flatten_conflict_with_top_level_tool_fails_closed_before_upstream` 绿（错误返回且 mock 上游收到 0 个请求） | ✅ 修复 |
| R06 | 单元红测试 T06a/T06b：schema `examples` 内 `function_call` 被扁平化改写；响应 `metadata` 内同名 JSON 被恢复器改写 | 扁平化改为 `flatten_responses_input_item_namespaces`（只处理 `input` 顶层调用 item）；恢复改为 `restore_responses_tool_namespaces` 只识别根级 item、`item`、`output`、`response.output` 四种协议形状；不再按 `type` 全树递归 | T06a/T06b 绿；`responses_namespace_tool_wire_round_trips_for_glm_and_openai` 等既有 round-trip 测试全绿 | ✅ 修复 |
| R07 | 单元红测试 I01：`item_demo` → `ct_demo` → `ctc_ct_demo` 双重规范化 | `normalize_responses_item_id` 跳过 `custom_tool_call`（交给专用规则）；`normalize_custom_tool_call_item_id` 只规范化 `fc_`/`item_` 前缀，原生 `ct_`/既有 `ctc_` id 保持原样；`call_id` 不改写 | I01（含幂等断言）与 I02（原生/既有 id 不被改写）绿；`foreign_item_ids_are_normalized_by_responses_item_type` 按统一规则更新（生产管线两函数按序 → `ctc_custom`，无叠加） | ✅ 修复 |
| R08 | 源码确认：`NativeAgentInterop::Auto ⇒ protocol==Responses`（非能力探测）；UI 文案「自动识别」暗示探测；缺少结构透传边界 | 1) UI/i18n 文案改为「Responses 默认启用」并在 hint 中明确「不做远端能力探测」；2) 新增 `responsesWirePolicy`（`compatible` 默认/`passthrough`），passthrough 跳过 reasoning/ID/原生子任务/additional_tools/namespace 全部结构改写，native 预处理同样被门控；模型路由、别名、认证仍按显式配置执行；缺失字段反序列化为 `compatible`，旧 profile 行为不变 | `relay_profile_responses_wire_policy_roundtrip`、`review_passthrough_policy_skips_structural_rewrites_but_keeps_alias` 绿；`npm run check` 通过 | ✅ 修复 |
| R09 | 风险核验项。核对结果：`passthrough`/`openAiOpaque`/`strip` 均为用户显式选择；`openAiOpaque` 仅清空 content、密文字节不动；`strip` 仅删 reasoning 条目；默认 `passthrough` 不丢历史。`openAiOpaque` 下无 `encrypted_content` 的 reasoning 被移除属于该显式策略的文档化语义（opaque 目标无法消费），非静默损失；诊断只记录计数/动作（`reasoning_diagnostic_detail_is_redacted` 验证不泄文本）。本轮不"解密"不伪造，未改代码 | — | 既有 reasoning 策略测试 + 审查测试全绿 | ✅ 核验通过 |
| R10 | 源码确认：launcher.rs Responses 流分支 chunk 读错误静默 `break` 后记 `stream_ok`；`finish()` 把半帧原始字节写给客户端（扁平名泄露）；空映射时逐帧解析重排（S08 违约） | `ResponsesNamespaceSseRewriter`/`NativeAgentSseRewriter` 新增 `finish_with_truncation`：完整但未终止的末帧照常恢复，半帧丢弃并返回截断标记；launcher 区分 `upstream_error`（记录 `helper.protocol_proxy_stream_failed`，发 SSE 注释行，不伪造成功）与 `truncated`；空映射时 `rewrite_frame`/`restore_responses_tool_namespace_json` 直接透传原始字节 | lib 全绿 + 既有 SSE round-trip/converter 测试全绿；S06/S07/S08 行为由新逻辑覆盖（故障注入单测见「未验证项」注记） | ✅ 修复 |
| R11 | 风险核验项。`complete_relay_profile_config_with_proxy` 的 loopback 回填/认证隔离由既有测试覆盖：`relay_config`（round-trip、`codex_plus_upstream_base_url` 元信息）、`provider_switch_isolation`、`launcher` 全绿，未发现回退证据。本轮不改代码 | — | 见测试结果表 | ✅ 核验通过（离线范围） |
| R12 | 源码确认：alias 替换位于 `RelayProtocol::Responses` 分支内，ChatCompletions 静默不生效 | alias 解析提到 `upstream_request_parts` 协议分支之前，按候选 profile 对两种协议一致生效；无循环 alias 链引入 | A04（fallback 两候选不同别名各自解析）绿；`glm_responses_moves_complete_tool_definitions_without_filtering` 等既有测试全绿 | ✅ 修复 |

## 兼容性

- **旧 profile 缺失新字段时的行为**：`responsesWirePolicy` 缺失 → 反序列化为 `compatible`，与审查 HEAD 的既有能力完全一致；未批量改写用户任何现有 profile。
- **compatible / passthrough 的边界**：compatible = 审查 HEAD 既有兼容能力 + 本次缺陷修复；passthrough = 跳过 reasoning/ID/原生子任务/additional_tools/namespace 结构改写（native 预处理一并跳过），模型路由、profile alias、认证、图片处理按显式设置照常执行——passthrough 不是 HTTP 字节级直连。
- **reasoning 有损策略**：`passthrough`/`openAiOpaque`/`strip` 全部保留，默认 `passthrough` 不变；有损均为显式 opt-in。
- **原生密文与跨供应商引用**：不解密、不伪造、不回写持久化历史；跨供应商服务端引用（`previous_response_id` 等）不支持迁移，行为与审查 HEAD 一致。
- **HTTP/JSON/SSE 路径**：三条返回路径的恢复顺序统一为「工具 wire name → namespace/name → native marker」；空映射时全部走字节透传 fast path。
- **R05 fail-closed 的影响**：以前"静默覆盖、最后一次获胜"的冲突请求现在会显式报错。这是任务书指定的契约（不能误派工具）；如果现网存在依赖错误行为的工作流，会从"悄悄认错工具"变为"明确失败"，属于有意暴露的问题。

## 构建与使用

- 产物：见 `REPAIR_TEST_RESULTS_20260916.md` 构建小节（产物位于 repair worktree `target/release` 与 `apps/codex-plus-manager/dist`，附 SHA256 与 build commit）。
- **未自动部署**：构建成功不代表已授权替换正在运行的应用。本任务未安装、未杀进程、未改用户活跃 profile。
- 回退方式：修复全部位于独立 worktree/分支 `fix/review-repair-20260916-234500`；不用即无影响。逐条 revert 对应提交即可，不涉及用户状态回退。

## 未验证项

| 项 | 状态 | 原因 |
|---|---|---|
| 真实上游 live 冒烟（官方/GLM/中转） | `NOT_RUN` | 任务书默认关闭 live 验证，未获用户明确开启 |
| `app_paths_resolves_portable_current_link_to_directory_version`（launcher 集成测试 1 项） | `BLOCKED` | Windows 创建目录符号链接需要管理员/开发者模式特权（os error 1314），与本次修复无关的纯环境限制 |
| R10 的 launcher 层故障注入端到端单测（S06/S07 以独立用例表达） | `PARTIAL` | R10 逻辑由 `finish_with_truncation` 承载并经既有 SSE 单测覆盖拆分字节/UTF-8 边界场景；launcher 层未新增模拟 TCP 半途断连的独立用例，列为后续补充项 |
| T09（空名/超长/非法字符工具名）、T10（additional_tools 重复声明时序契约）、T11（tool_choice/allowed-tools 全覆盖） | `NOT_RUN` | 本轮按任务书聚焦 fail-closed 冲突与字段保留；空名跳过沿用既有行为未改，其余需要先导出实际客户端 schema 再定契约 |
| §3.5 provider fingerprint 缺口验证 | `NOT_RUN` | 指纹包含 profile id/name/relayMode/provider/config provider；任务书要求"只有测试证明有缺口才改"，本轮未构造出失效场景，保持原样 |
| Codex app-server schema 导出核对（§2.4） | `NOT_RUN` | 未执行 `codex app-server generate-json-schema`；F10 的 openai/自定义 provider 断言基于本仓库既有 `applyCodexRemoteSessionProviderOverride` 契约而非导出 schema |

## 与任务书硬约束的对照

- 未使用 `git reset --hard`/`git clean`/强推（worktree 内对**本地未推送提交**做过一次软重置以整理提交顺序，工作区文件从未丢弃）。
- 未删除/清洗任何 SQLite、rollout JSONL、会话数据；未用删会话让测试通过。
- 未强制单供应商、未取消回退、未禁用工具、未关闭原生子任务。
- 未删除旧测试、未放宽断言；唯一的既有断言修改是 `foreign_item_ids_are_normalized_by_responses_item_type`（旧断言体现的是 R07 确认的双重规范化规则，按统一规则更新并保留全部保护意图，详见代码注释）与 service-tier harness 的 messageBus mock（补上真实 host 必有的 `mcp-response` 回复，契合新契约）。
- 未提交任何密钥/密文/敏感正文；`.review-local/` 日志不入库。
- 未访问真实付费上游；全部为 loopback mock。
