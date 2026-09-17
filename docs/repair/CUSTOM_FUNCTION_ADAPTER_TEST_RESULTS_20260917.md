# Custom-Function 适配器测试结果（2026-09-17）

所有命令在 worktree `E:\Code\CodexPlusPlus-repair-20260916-234500`、分支 `fix/custom-tool-function-adapter-20260917`（915ce33a）执行。Rust 工具链 rustc/cargo 1.98.1（~/.cargo/bin）；管理器 .ts 测试使用便携 Node v22.18.0（`.review-local/tools/`，隔离安装，未改全局环境）。

## 套件结果

| 套件／命令 | 修复前 | 修复后 | 退出码 | 证据 |
|---|---|---|---|---|
| `cargo test -p codex-plus-core --lib`（含 18 个 custom_tool_adapter 新单测 + settings round-trip） | 单测新增 | **368 passed / 0 failed**（适配器 18 + 全量） | 0 | 会话内执行记录；初版 7 项红（SSE 组缺失/尾部垃圾未拒绝/终态误交付）后转绿 |
| `cargo test -p codex-plus-core --test protocol_proxy` | 新增 | **100 passed / 0 failed**（含 10 个适配器集成测试） | 0 | 同上 |
| `cargo test -p codex-plus-core --test launcher` | — | 84 passed / 1 failed（`app_paths_resolves_portable_current_link_to_directory_version`：os error 1314，Windows 符号链接特权，**既有环境限制**，与上轮报告记录一致） | 1（既有） | 同上 |
| `cargo test --workspace --no-fail-fast`（cargo clean 后全量重建） | — | **650 passed / 1 failed**（唯一失败同上） | 1（既有） | 后台执行日志 |
| `cargo test -p codex-plus-manager`（src-tauri，含 CLI 新测试） | 新增 | **65 + 22 passed / 0 failed**（`provider_update_accepts_custom_tools_as_functions` 绿） | 0 | 会话内执行记录 |
| `tsc --noEmit -p tsconfig.json`（npm run check 等价） | — | 0 错误 | 0 | 同上 |
| `node --test src/*.test.ts`（Node 22 便携版） | — | **130 passed / 0 failed** | 0 | 同上 |
| `vite build` | — | 成功（3.19s；chunk 体积警告为既有提示） | 0 | 同上 |

## 新增行为测试矩阵（对应任务书 §13.4）

| ID | 测试（测试函数／位置） | 验收断言 | 结果 |
|---|---|---|---|
| C01 | `relay_profile_custom_tools_as_functions_roundtrip`（settings.rs）+ `custom_adapter_off_keeps_custom_wire_form` | 缺字段=false；关闭时 wire 保持 custom 原形 | ✅ |
| C02 | `provider_update_accepts_custom_tools_as_functions`（cli.rs） | CLI 字段级 true→false 写回一致 | ✅ |
| C03 | `custom_adapter_conflicts_with_passthrough_and_fails_before_upstream` + `customAdapterPolicyConflict`（App.tsx 保存门） | 后端 `CUSTOM_ADAPTER_POLICY_CONFLICT`、上游 0 请求；UI 拦截保存 | ✅ |
| Q01/Q02/Q05/H01/H02/H07 | `custom_adapter_on_wraps_declarations_history_and_tool_choice` | 声明/历史/tool_choice 包装、普通 function 不动、原请求不改写 | ✅ |
| Q04/Q06 部分 | namespace 扁平化复用既有 R04/R05 机制 + 同名不同身份（`functions__review_echo` 与顶层 `review_echo` 并存） | 两种身份独立包装与还原 | ✅（JSON 端到端测试覆盖） |
| Q07 | `grammar_meta_is_preserved_as_description_note` | grammar 语法并入包装描述 | ✅ |
| J01/J03/J06 | `custom_adapter_json_response_restores_custom_tool_call_end_to_end` | input 逐字符还原（含 CRLF/引号/emoji/空串）、arguments 移除、回显 tools/tool_choice 还原、`functions__review_wait` 不误转 | ✅ |
| J02/J05 | `unmanaged_function_call_in_response_passes_through` + `json_restore_skips_plain_functions_and_metadata` | 普通 function/业务 JSON 不被扫描改写 | ✅ |
| J04 | `invalid_arguments_fail_closed` | 缺字段/null/数字/数组/额外字段/重复键/尾部垃圾 → `CUSTOM_ADAPTER_INVALID_ARGUMENTS`；合法空串成功 | ✅ |
| S01/S02/S03/S04 | `sse_buffers_and_delivers_custom_group_with_sequence_renumbering`（7 字节分片喂入，JSON escape 与 UTF-8 跨 chunk） | 事件组语义、sequence 单调、call_id 保持 | ✅ |
| S05/S06 | `sse_inconsistent_delta_and_done_fails` + added/done 身份一致性代码路径 | 不一致显式失败，不猜测 | ✅ |
| S07 | `sse_eof_with_pending_emits_error_not_success` / `sse_half_frame_at_eof_is_reported_truncated` | EOF 半帧/未交付调用：发 `error` 事件，不伪造成功 | ✅ |
| S09 | 重复 done 一致性分支（`handle_output_item_done` delivered_input 路径） | 不重复派发、内容不一致显式失败 | ✅（单测+代码路径） |
| S10 | `sse_incomplete_terminal_never_delivers_input` | incomplete 终态不交付可执行输入 | ✅ |
| S12 | `empty_plan_is_byte_fast_path` | 空计划逐字节透传 | ✅ |
| P01 | `custom_adapter_json_response_restores_custom_tool_call_end_to_end`（JSON）/ `custom_adapter_buffered_sse_restores_custom_events`（缓冲 SSE）/ `custom_adapter_real_socket_streaming_delivers_custom_events`（**真实 socket，mock 上游分 3 片写入**） | 三入口语义一致 | ✅ |
| P02 | `custom_adapter_chat_upstream_restores_namespace_custom_end_to_end` | Chat 上游 namespace custom 包装+还原、converter 上下文正确 | ✅ |
| P03 | 既有 100 个 protocol_proxy 测试 + 650 workspace 测试全绿 | Chat 关闭路径零回归 | ✅ |
| F01/F02 | `custom_adapter_fallback_from_enabled_to_disabled_is_isolated` / `..._disabled_to_enabled_...` | 候选各自独立编码，互不污染 | ✅ |
| 状态引用 | `custom_adapter_rejects_server_state_references` + `state_reference_detection` | previous_response_id/conversation/item_reference → `CUSTOM_ADAPTER_STATE_REFERENCE_UNSUPPORTED` | ✅ |
| 精确向量 §13.3 | `exact_input_vectors_round_trip` | 9 组向量逐字符相等 | ✅ |

统计说明：`custom_tool_adapter` 模块单测 18 项、协议集成测试 10 项；S08（completed-only 精简流）由 `handle_response_terminal` 的未交付分支与 J01（终态 output 还原）覆盖；S11（缓冲超限）为常量检查代码路径，未做 8 MiB 级实测（见主报告未验证项）。
