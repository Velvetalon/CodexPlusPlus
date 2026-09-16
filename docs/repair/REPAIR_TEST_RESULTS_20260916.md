# CodexPlusPlus 修复测试结果（2026-09-16）

环境：Windows 11 / rustc-cargo 1.98.1 (stable-x86_64-windows-msvc) / Node v20.11.1（系统，无法运行 `node --test "src/*.test.ts"`，`.ts` 支持需 Node ≥22.6）+ Node v22.18.0 便携版（仅用于前端测试，未改系统）/ codex-cli 0.133.0。
日志目录：`.review-local/`（不入库）。

## 修复前红测试证据（全部为真实生产路径/生产函数，非冻结副本）

| 测试 | 套件 | 修复前表现 | 修复后 |
|------|------|------------|--------|
| `review_namespace_function_preserves_explicit_fields`（T01/R04） | lib | 红：顶层 `strict:false` 丢失（None） | 绿 |
| `review_schema_examples_are_not_protocol_items`（T06a/R06） | lib | 红：`parameters.examples` 内 `function_call` 被改写为 `business__record` | 绿 |
| `review_response_metadata_is_not_a_tool_call`（T06b/R06） | lib | 红：`metadata` 内同名 JSON 被恢复器插入 namespace、改写 name | 绿 |
| `review_custom_item_normalization_has_one_canonical_rule`（I01/R07） | lib | 红：`item_demo` → `ctc_ct_demo` | 绿 |
| `namespace_flatten_conflict_with_top_level_tool_fails_closed_before_upstream`（T04/R05） | protocol_proxy 集成 | 红：冲突请求照发上游（mock 收到请求、返回 200） | 绿：发送前报错，上游收到 0 个请求 |
| `aggregate_fallback_does_not_inherit_first_candidate_native_agent_rewrites`（A01/R03） | protocol_proxy 集成 | 红：候选 B 继承候选 A 的 `encrypted:false` 明文标记 | 绿 |
| `aggregate_fallback_second_candidate_applies_its_own_native_prep_and_restore`（A02/R03） | protocol_proxy 集成 | 红：候选 B（interop on）未执行自己的别名预处理 | 绿 |
| `injection_script_blocks_turn_until_thread_binding_confirmed` F01（R01） | cdp_bridge | 红：resume 抛错后 `turn/start` 照发（turn=1）、缓存写入成功 | 绿（F03/F09 等守卫场景同步验证时序契约） |
| `injection_script_blocks_turn_until_thread_binding_confirmed` F02（R01） | cdp_bridge | 红：失败后不再重试 resume（尝试次数=1） | 绿（=2） |
| `injection_script_blocks_turn_until_thread_binding_confirmed` F04（R02） | cdp_bridge | 红：bus dispatch resolve 即缓存成功、发 `thread_model_context_refreshed`、投递原始 turn | 绿 |

## 修复后回归结果（修复提交 HEAD=423546a）

### Rust

| 命令 | 结果 | 退出码 |
|------|------|--------|
| `cargo test -p codex-plus-core --lib` | 348 通过 / 0 失败（基线 340 通过 + 8 个新审查测试） | 0 |
| `cargo test -p codex-plus-core --test cdp_bridge` | 114 通过 / 0 失败（113 既有 + 1 新契约；基线为 113 通过 + 新契约红） | 0 |
| `cargo test -p codex-plus-core --test protocol_proxy` | 90 通过 / 0 失败（88 既有 + T04/A01/A02；另 A04/A08 已含于 90 内） | 0 |
| `cargo test --workspace --no-fail-fast` | **1122 通过 / 1 失败 / 2 忽略，40 个测试目标中 39 个全 ok**；唯一失败为环境阻塞项（符号链接特权），详见下表 | 101（因该环境失败） |

### `cargo test --workspace --no-fail-fast` 分套件明细

（数据取自 `.review-local/workspace-test4.log`，见下方汇总；0 失败的套件不再逐条展开）

| 套件 | 通过/失败/忽略 |
|------|----------------|
| codex-plus-core lib | 348/0/0 |
| 其余 `--test` 目标（ads、bridge_routes、cdp_bridge、codex_app_state、codex_sqlite、dream_skin×5、floating_panel×2、force_chinese_locale_settings、installers、protocol_proxy、provider_switch_isolation、relay_config、renderer_inject 等） | 全部 0 失败（其中 2 个既有 ignored 用例维持 ignored） |
| launcher（唯一例外） | 84 通过 / **1 失败** / 0 忽略 |
| 失败项 | `app_paths_resolves_portable_current_link_to_directory_version`：`std::os::windows::fs::symlink_dir` 需要管理员/开发者模式特权（os error 1314「客户端没有所需的特权」）。该测试与本仓库 9/15、9/16 两次提交及本次修复内容均无交集，属环境特权阻塞（`BLOCKED`），非本次改动引入 |

### 前端（apps/codex-plus-manager）

| 命令 | 结果 | 退出码 |
|------|------|--------|
| `npm run check`（tsc --noEmit） | 无类型错误 | 0 |
| `npm test`（Node v22.18.0 便携版） | 130 通过 / 0 失败 / 0 跳过（12 suites） | 0 |
| `npm test`（系统 Node v20.11.1） | `ERR_UNKNOWN_FILE_EXTENSION`（.ts 需 Node ≥22.6）——工具链阻塞，非测试失败 | 1 |
| `npm run vite:build` | 构建成功（built in 2.97s） | 0 |

### 说明

- 所有计数为测试框架报告的实际数字（passed/failed/ignored），非仅退出码。
- F01-F14、A01-A04/A08、T01/T04/T06/I01/I02/T12 的断言全部针对生产入口：前端经 `assets::injection_script(57321)` 写盘后由 Node 加载真实脚本；Rust 侧调用 `open_responses_proxy_request_with_settings` 与 `upstream_request_parts` 生产函数，loopback mock 全部监听 `127.0.0.1:0` 临时端口。
- 未执行真实上游请求（`NOT_RUN`，未获 live 授权）。

## 构建

| 产物 | 状态 | SHA256 |
|------|------|--------|
| `target/release/codex-plus-plus.exe`（launcher，18,185,728 字节，build commit `423546af78fa7efc99f4e54c9c98f47bcc28ba7f`） | 构建成功（BUILD_EXIT=0，release profile 47.48s） | `b9aad01cb35b32e7103efbac14e44c4ac0d8eedf2a9ac39b45cf8deb5894aa93` |
| `apps/codex-plus-manager/dist`（manager 前端静态资源，`npm run vite:build`） | 成功（built in 2.97s） | 目录产物，未取单值 |

产物保留在 repair worktree 内。**构建成功不代表已授权替换正在运行的应用**：未安装、未杀进程、未修改用户活跃 profile。
回退：全部改动在独立分支 `fix/review-repair-20260916-234500`，不用即无影响；需要时可对 4 个修复提交逐条 revert。
