# CodexPlusPlus：Custom ↔ Function 双向适配修复报告（2026-09-17）

> 任务书：《CodexPlusPlus：Custom ↔ Function 双向适配修复与 A/B 验收方案（2026-09-17）》
> 结论速览：**适配器已实现并通过全部离线回归；同通道真实 A/B（AIHubMix，16 个真实请求）显示 B 组（开关开启）12/12 完整闭环、A 组（现状）0/4 产生正确调用。** 实际 Codex 桌面端验收 `NOT_RUN`（见「未验证项」）。整体状态：**PARTIAL（工程修复与 live A/B 完成；实际 Codex 验收未执行）**。

## 主报告必填项

```text
实际起始提交／结束提交：
  起始 c4ed99a5（远端 fix/review-repair-20260916-234500 最新 tip，与任务书锚点一致）
  结束 915ce33a（分支 fix/custom-tool-function-adapter-20260917，7 个提交）
实际工作区：
  E:\Code\CodexPlusPlus-repair-20260916-234500（worktree，工作分支
  fix/custom-tool-function-adapter-20260917；主仓库 E:\Code\CodexPlusPlus 的 main 未动）
实际构建产物与 SHA256（build commit 4a57ba05 之后追加警告清理，
产物对应清理后源码；tari.conf.json bundle.active=false，单 exe 直接分发，未部署）：
  target/release/codex-plus-plus.exe         18,370,560 字节
    SHA256 76369c197b6a0605ae69d3a5ef52359f063480ff0205bda4178c4889c680f030
  target/release/codex-plus-plus-manager.exe 39,110,144 字节（含前端 dist）
    SHA256 d75a410319a581548d2b52d9b9cb8434bf3d8633d60e214e27885887b0cc452e
  离线回归与 A/B 均以测试进程内驱动生产函数链，与产物构建同源。
真实运行的是哪份程序：
  live A/B 由 cargo 测试进程内调用生产函数（open_responses_proxy_request /
  handle_responses_proxy_request / custom_tool_adapter），非桌面端安装版。
原故障是否复现：
  是。同通道 A 组（适配关闭，即现状行为）4 次首轮请求全部失败：
  3 次「上游不产生任何工具调用、只回文本」，1 次「返回 custom_tool_call 但 input 为空」。
工具在哪一跳／哪类契约失败：
  上游（AIHubMix Responses 入口）对 custom 类型工具声明的消费与回传：
  声明被回显但模型不产生调用；偶发产生的调用丢失 input 字符串。
  属于传输格式契约问题，不是本地执行器问题。
新字段名、默认值、UI位置：
  customToolsAsFunctions（Rust: custom_tools_as_functions），默认 false；
  供应商编辑页 → 更多选项 →「Custom 工具转 Function」开关，与 Responses
  wire 策略相邻；passthrough 组合在保存前显式拦截。
实际AIHubMix目标profile与模型ID（脱敏）：
  profile relay-mu4eaey2（名称 AIHubMix），origin https://aihubmix.com/v1，
  API 路径 /responses；模型 coding-glm-5.3-flash 与 coding-glm-5.3。
  凭据引用：该 profile authContents.OPENAI_API_KEY（仅内存/隔离副本使用，未入库未打印）。
是否改变了任何供应商／凭据／模型／付费通道：
  否。真实 settings.json 未被修改（只读取证）；A/B 使用 .review-local/ab/ 下的
  隔离设置副本（已被 .gitignore 排除）。生产活跃路由仍是「本地 8787/8788 订阅绕路」
  状态，与任务前一致；恢复到 AIHubMix 属于需用户确认的操作，未执行。
双向转换支持的边界：
  - Responses wire：custom 声明/历史/tool_choice → 单 {input:string} function 包装；
    响应 function_call 严格解码还原（拒绝未知字段/重复键/非字符串/尾部垃圾）。
  - grammar：原 format 元信息保留并并入包装描述（文字指导），服务端约束采样退化为
    本地校验，明确记录，不冒充无损。
  - Chat Completions：顶层 custom 沿用既有转换（不受开关影响）；开关开启时额外
    提升 namespace 内 custom 子工具（单层 namespace），解码沿用既有 Chat 重建语义。
  - 服务端引用（previous_response_id/conversation/item_reference）：开关开启时
    显式报 CUSTOM_ADAPTER_STATE_REFERENCE_UNSUPPORTED，要求完整内联历史。
  - 聚合容器 profile 不使用容器级开关（成员各自生效）；code_mode_host 若过滤
    custom 工具，适配器无法恢复被删声明（如实限制）。
对grammar／流式时延／服务端引用的限制：
  受管调用的流式参数按调用缓冲、完整校验后一次性交付（可能略晚显示）；
  活跃流统一分配单调 sequence_number；服务端引用见上。
JSON／SSE／历史回放结果：
  全部通过（见测试结果文档）；含真实 socket 分片流式测试。
实际Codex终端与文件编辑结果：
  NOT_RUN（见「未验证项」）。
CUA独立问题状态：
  未触碰（unsupported Codex auth method: apikey 属独立问题，不在本任务范围）。
已部署／仅构建／隔离实例：
  未部署、未替换任何可执行文件、未重启任何生产进程。live A/B 为进程内生产链。
回退方法：
  分支级：不合并/不部署即无影响；配置级：将对应供应商的 customToolsAsFunctions
  置为 false（存储/CLI/GUI 均支持），恢复开启前行为；无全局副作用。
未验证项：见文末。
```

## 提交清单（c4ed99a5 → 915ce33a）

| 提交 | 内容 |
|---|---|
| 5f551129 | feat(settings): per-provider custom-as-function option（字段/默认/导入/CLI/round-trip 测试） |
| 99320197 | feat(proxy): custom_tool_adapter 模块（严格编码/解码/JSON 还原/SSE 状态机） |
| d31417cf | fix(proxy): 请求级计划 + 四个返回入口接入 + Chat namespace 组合 |
| b50cbe41 | feat(manager): 更多选项开关 + 冲突拦截 + i18n |
| eac28dfe | test: 请求/JSON/缓冲SSE/真实socket/候选隔离等 10 个集成测试 |
| 637cc0d6 | chore: .review-local 本地修复工作区不入库 |
| 915ce33a | test: opt-in live A/B harness（生产代理链，预算上限） |

## 修复前红证据（行为级）

1. **线上红（决定性）**：同通道 A 组（= 基线行为）在真实 AIHubMix 上 4 次首轮全部失败——custom 声明发出后模型不产生工具调用（3 次），或产生调用但 input 为空（1 次）。详见 A/B 报告。
2. **离线红**：基线管线对 custom 声明原样透传（`custom_adapter_off_keeps_custom_wire_form` 断言的正是该缺口行为），且响应侧对 function_call 无还原路径（新增的还原测试在基线代码上无对应实现可走通）。
3. **开发过程红**：custom_tool_adapter 模块单测首轮 7 项失败（SSE 事件组缺失、严格解码未拒绝尾部垃圾、终态误交付等），修复后全绿；过程输出保留在会话记录。

## 设计与实现要点

- 请求级 `CustomToolAdapterPlan` 随 `UpstreamProxyResponse` 返回，候选失败不污染后续候选（复用并延续 R03 的隔离框架）；无任何全局可变状态。
- 响应还原顺序（§7.3 逆序）：custom 解包 → namespace 恢复 → native 标记恢复；四个返回入口（pp JSON / pp 缓冲 SSE / launcher 实时 SSE / launcher JSON）语义一致。
- 严格解码：手工 serde visitor，恰好一个 `input` 字符串字段；`{"input":"a","input":"b"}`、`{...} trailing`、非对象 JSON 一律 `CUSTOM_ADAPTER_INVALID_ARGUMENTS`。
- SSE：只缓冲受管调用（PendingCall 状态机），普通文本/reasoning/普通 function/心跳原样流动；三方一致性核对在解码值层面（容忍合法转义差异）；`incomplete/failed` 终态不交付可执行输入；EOF 有未交付调用时发官方 `type:"error"` 事件并携带 `CUSTOM_ADAPTER_STREAM_INCOMPLETE`。
- 缓冲上限：单调用 8 MiB、单响应合计 32 MiB、并发 128 调用，超限类型化失败。
- 开关关闭时全部入口为字节透传 fast path（有专门测试守护）。

## 未验证项

| 项 | 状态 | 原因 |
|---|---|---|
| 实际 Codex 桌面端闭环（终端/apply_patch/下一轮，§15.2） | `NOT_RUN` | 需要隔离实例或生产重启授权；桌面端单实例机制无法保证测试实例不接管生产会话，任务书 §15.1 要求先停手并请求确认。 |
| GLM→Luna 切换冒烟（§15.2-7） | `NOT_RUN`（离线证据：reasoning 策略路径零改动，`C01` 回归护栏通过） | 同上，需真实会话。 |
| 生产路由恢复到 AIHubMix（§15.4） | `BLOCKED_AUTH_CONFIRMATION` | 当前活跃路由在「本地 8787→8788 订阅」绕路状态；恢复会改变用户生产计费路径，须用户确认后执行。 |
| 两模型的完整 3 组配对（glm-5.3 仅 1 组） | `PARTIAL` | 预算控制：flash 跑满 3 组，glm-5.3 做 1 组跨模型验证即达目的。 |
| code_mode_host 与适配器共存 | 设计核验（过滤器先于适配器运行，过滤后无法恢复；聚合 codeModeHost 当前为 false，不影响现网） | 无现网触发场景。 |
| 缓冲上限调优（8/32 MiB） | 按方案默认值，未做现场容量测试 | 现网无超限样本。 |

## 回退

1. 配置级：目标供应商 `customToolsAsFunctions` 设为 false（GUI 关闭开关、或 CLI `provider-update` 写 false）→ 立即恢复开启前行为；其他字段不动。
2. 代码级：分支 `fix/custom-tool-function-adapter-20260917` 未合并/未部署即无影响；已部署场景按提交逐个 revert。
3. 注意：关闭开关会重新暴露 AIHubMix 的 custom 兼容问题（即 A 组行为）——这是回到基线，不是开关失效。
