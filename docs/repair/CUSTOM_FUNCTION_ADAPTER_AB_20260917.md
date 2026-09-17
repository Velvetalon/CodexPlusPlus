# 同通道真实 A/B 报告：Custom-as-Function 适配（2026-09-17）

## 实验问题

> 对同一个已核实的 AIHubMix 入口、同一个 Key 引用和同一个允许访问的模型 ID，custom 包装开关是否改善了真实工具调用与结果回放，同时保持客户端输入语义？

## 实验设计

- **通道固定**：profile `relay-mu4eaey2`（AIHubMix），origin `https://aihubmix.com/v1`，API 路径 `/responses`，同一份凭据引用（authContents.OPENAI_API_KEY，仅隔离副本使用）。**未使用智谱订阅或本地 8788 通道。**
- **A/B 两臂只差一个字段**：`customToolsAsFunctions` = false（A，现状行为）/ true（B，新适配器）。请求编码与响应解码全部经过**生产实现**（`handle_responses_proxy_request` 完整入口 + `custom_tool_adapter`），无任何临时字符串函数。
- **canonical 历史相同**：每对试验的两臂使用同一 fixture 与同一测试向量；回放轮携带相同的 canonical custom 视图历史。
- **synthetic 工具** `review_echo`：只做字符串验收（对比期望向量），返回 `{runId, inputSha256, resultNonce}`；控制工具 `review_wait`（普通 function）。不触 shell/eval。
- **采样**：`tool_choice: "auto"`（两臂一致）+ 固定提示词强制调用 review_echo；`max_output_tokens: 4096`；单请求超时 90s；AB/BA 交替。
- **harness**：`cargo test -p codex-plus-core --test protocol_proxy live_ab_ -- --ignored`（opt-in，默认 `#[ignore]`；预算上限 `CODEXPLUS_AB_MAX_REQUESTS`）。
- 隔离：设置副本位于 `.review-local/ab/`（已 gitignore）；真实 settings.json 未修改；进程经本机代理出网（与生产环境网络拓扑一致）。

## 结果汇总

主运行 runId `ab-1789668078471`（预检运行 `ab-1789668036383`，2 请求，1 配对，结果与主运行一致）。

| 模型ID | 模式 | A 有效调用/次数 | B 有效调用/次数 | B 输入精确匹配 | B 结果回放 | 路由一致性 | 结论 |
|---|---|---|---|---|---|---|---|
| coding-glm-5.3-flash | JSON | 0/1 | 1/1 | 1/1 | 1/1 | 同 origin/同凭据引用/同模型（构造保证） | 适配改善明确 |
| coding-glm-5.3-flash | SSE | 0/2（其中 1 次返回调用但 input 为空） | 2/2 | 2/2 | 2/2 | 同上 | 实时路径同样改善 |
| coding-glm-5.3 | JSON | 0/1 | 1/1 | 1/1 | 1/1 | 同上 | 跨模型一致 |

- **A 组（现状行为）0/4**：3 次「上游不产生任何工具调用，只回文本」；1 次（SSE）返回了 `custom_tool_call` 但 **input 为空串**（期望 20 字符向量）——证实该入口对 custom 传输的消费存在两类损坏：调用缺失与输入丢失。
- **B 组 4/4 首轮 + 4/4 回放全部 PASS**：调用产生、客户端收到 `custom_tool_call` 且 input 与期望逐字符相等、call_id 配对有效、下一轮模型正确返回运行时生成的 resultNonce（非提示词已知信息）。
- 任务书 §14.9 对照：符合第 1 行「A 不产生正确调用，B 完整闭环」→ **支持「该入口在当前条件下需要适配」**；不宣称已看穿服务商内部实现。

## 预算与用量

| 项 | 值 |
|---|---|
| 本次任务真实请求总数 | 16（预检 2 + 3 回放计入；主运行 budgetUsed=13，上限 30） |
| 上报 usage 的请求 | 8 条：合计 input 2,076 / output 2,064 tokens（其余响应未回传 usage 字段） |
| 401/403/402 | 0 |
| 429/超时 | 0 |
| 费用 | 未核实账单金额（仅 token 计数；未做任何费率换算） |

## 每次试验记录

完整字段（§14.8 形状：runId/arm/stream/httpStatus/upstreamCallType/clientCallType/inputExactMatch/callId/resultNonce/latencyMs/usage/result/note）保存在：

- 汇总：`.review-local/ab/results/ab-summary.json`（本地脱敏目录，不入库）
- JSONL：`%TEMP%\codexplus-ab-<runId>.jsonl`

## 限制

- 无法验证 AIHubMix 内部是否同模型实例；结论限定为「同入口、同请求模型 ID、同凭据引用」。
- 小样本（4 配对）足以作工程验收，不证明服务商永久、全面不支持 custom。
- B 组的 wire 侧证据由离线测试链证明（mock 上游捕获断言 function 包装），live 运行通过生产编码/解码链的端到端行为（调用产生+精确往返+回放闭环）间接证明包装生效。
