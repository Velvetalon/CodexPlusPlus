# OpenCode Go 接入 Codex 的验证记录（2026-09-22）

状态：**未完成接入，当前阻塞在账户额度（非协议）**。
下次开通 / 恢复 OpenCode Go 后，按文末「验证清单」继续。

## 1. 结论速览

| 结论 | 状态 | 依据 |
|---|---|---|
| OpenCode Go 的 /zen/go/v1/responses **接受 DeepSeek** | 已确认 | 乱写的模型名返回 ModelError，DeepSeek 只返回 CreditsError → 模型与格式校验已通过，只剩账务 |
| 直连 responses 不需要本地协议代理 | 已确认 | 与参考文章一致，Codex 可直接用 wire_api = "responses" |
| 需要 x-opencode-session 会话头 | 取决于时点 | 2026-09 文档要求；实测缺该头时返回 400 MissingSessionID |
| 当前能否真正调用 | **不能** | 所有 Go 模型 × 两种端点 × 有无会话头 → 全部 401 CreditsError |

## 2. 端点与格式（官方文档 + 实测）

| 模型 | Go 端点（文档给的 AI SDK 组合） |
|---|---|
| deepseek-v4.1-flash / v4-pro / v4-flash、glm-*、kimi-*、qwen*、minimax-*、mimo-* | https://opencode.ai/zen/go/v1/chat/completions |
| grok-4.7 / grok-4.6 / gpt-5.6-luna | https://opencode.ai/zen/go/v1/responses |

注意：文档的端点表是「推荐组合」，不是排他限制。实测 /zen/go/v1/responses 也接受 DeepSeek
（参考文章即用该路径拿到 200 completed）。

## 3. 关键时间线（2026-09-22，本地时间）

| 时间 | 操作 | 结果 |
|---|---|---|
| 09:4x | manager 供应商测试：opencode profile（当时 responses + /zen/v1）+ deepseek-v4.1-flash | 401 ModelError: Model deepseek-v4.1-flash is not supported for format openai |
| 10:28–10:34 | /zen/go/v1/chat/completions + deepseek-v4.1-flash + 会话头（带 key） | **200，返回真实内容**（说明 key、订阅、格式当时都可用） |
| 10:5x–11:0x | 同一 key、同一端点、同样简单请求 | **401 CreditsError: Insufficient balance** |
| 11:0x | /zen/go/v1/responses + deepseek-v4-flash / v4.1-flash（带/不带会话头） | 401 CreditsError |
| 11:0x | /zen/go/v1/responses + 乱写模型名 | 401 ModelError: Model ... is not supported（关键对比项） |
| 11:0x | glm-5.3-flash / kimi-k2.7-code / minimax-m3 / qwen3.8-flash / gpt-5.6-luna | 全部 401 CreditsError |

结论：同一套请求在 20 分钟内从 200 变成 CreditsError，且覆盖所有模型与端点，
**变化发生在账户侧（额度/订阅），不在请求形状**。

## 4. 账务规则（官方文档摘要）

- Go 是 10 美元/月订阅，按模型给「月度美元额度」；5 小时 = 20%、周 = 50%、月 = 100%。
- 额度用尽后：如果 console 里开启了 Use balance，会**回落到 Zen 余额**继续服务；
  未开启或余额不足则直接阻止请求。
- 因此 CreditsError 的最可能原因：Go 额度已用尽 + Zen 余额为空。
- 待确认项：Go 是否在该 workspace 生效（一个 workspace 只允许一名成员订阅）、
  当前 5h/周/月余量、Use balance 开关与 Zen 余额。

（workspace 标识已脱敏为 wrk_01M3…；完整值见报错原文与管理台。）

## 5. 参考文章给出的 Codex 侧适配要点

来源：https://lucaslz.com/tools/codex-opencode-go-deepseek-v4-flash/ （2026-08-06）

1. 直连 Responses：config.toml 里自定义 provider 用 wire_api = "responses"，
   base_url 指向 https://opencode.ai/zen/go/v1，不需要本地协议代理。
2. **模型目录的 tool_mode 不能是 code_mode_only**：作者补上自定义目录后，
   code_mode_only 导致 Codex 的 exec 工具声明被上游拒绝；改成标准模式后恢复正常。
   这是全流程里最反直觉、也最容易复现的一个坑。
3. API Key 不写进 config.toml，运行时从 OpenCode 已保存的认证文件读取。
4. 模型目录需要提供上下文窗口与推理档位，否则 Codex 走「未知模型」回退（能跑但有告警）。
5. 用两个 profile 切换（例如 -p deepseek / -p openai），切换时 provider 与目录一起换。
6. 验收方式：跑真实对话，并让 DeepSeek 通过 apply_patch 创建文件并做字节级核对。

## 6. 本轮做过、随后按要求回滚的改动

| 内容 | 提交 / 状态 |
|---|---|
| Codex++ 会话头转发（session_id / x-opencode-session → x-opencode-session，回落 prompt_cache_key） | 6738b11 → 已 revert（f707d42） |
| 发布二进制（v25） | d8b6994 发布，随后 f2ad591 恢复为 v24 二进制 |
| 稳定版v25 目录 | 已删除（v20–v24 保留） |
| opencode profile 配置 | 曾改为 chatCompletions + /zen/go/v1，已还原为 responses + /zen/v1 |

回滚原因：用户判断「chat → responses 的适配没必要做」。
按参考文章，这个判断可以撤销——直连 responses 本来就不需要任何协议转换。

## 7. 下次开通 Go 后的验证清单

1. 控制台确认：Go 在该 workspace 生效、5h/周/月余量与 Zen 余额、Use balance 开关。
2. 最小请求（不要写 key 进脚本）：
   POST https://opencode.ai/zen/go/v1/responses
   body: {"model":"deepseek-v4-flash","input":"Reply exactly OK.","max_output_tokens":16}
   期望：HTTP 200，status=completed，output_text=OK。
3. Codex++ profile：upstreamBaseUrl = https://opencode.ai/zen/go/v1，protocol = responses，
   模型列表补上 deepseek-v4-flash / deepseek-v4.1-flash。
4. 切换后检查生成的模型目录：tool_mode 必须是标准模式（不能是 code_mode_only），
   上下文窗口与推理档位是否正确。
5. Codex 里跑真实回合：让 DeepSeek 用 exec / apply_patch 写一个文件，核对文件内容与结尾换行。
6. 如果报 400 MissingSessionID：把客户端 session_id 转成 x-opencode-session 转发
   （本轮实现过，revert 在 f707d42，可以直接 cherry-pick 6738b11 回来）。
7. 原始探测记录见同目录 raw-probes-20260922.md。

## 8. 与官方 DeepSeek 线路的差异（不要互相套用）

| 线路 | 协议要求 | 工具模式 |
|---|---|---|
| 官方 DeepSeek（api.deepseek.com/v1/responses） | responses | code_mode_only = true + custom 工具包装成 function 才通 |
| OpenCode Go（opencode.ai/zen/go/v1/responses） | responses | 按参考文章需**标准** tool_mode |

两条线的要求相反，改动前先确认当前 profile 属于哪一条。
