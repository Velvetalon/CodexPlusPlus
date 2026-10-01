# OpenCode Go 原始探测记录（2026-09-22）

所有请求都用同一条 opencode profile 里已保存的 key 发出；
key 只读进 PowerShell 变量，从未打印、未写入任何文件。

## 0. 取 key（不落地）

    $m = '<Codex++ 安装目录>\codex-plus-plus-manager.exe'
    $sec = (& $m --cli provider-get --input '{"id":"relay-muc0n4hz"}' --include-secrets | ConvertFrom-Json).provider
    $auth = $sec.authContents | ConvertFrom-Json
    $key  = $auth.OPENAI_API_KEY
    if (-not $key) { $key = ($auth.PSObject.Properties | Select-Object -First 1).Value }

## 1. 端点 / 格式判定（不带 key 也能复现前两条）

    POST https://opencode.ai/zen/v1/responses            {"model":"deepseek-v4.1-flash","input":"hi"}
    POST https://opencode.ai/zen/v1/chat/completions     {"model":"deepseek-v4.1-flash","messages":[{"role":"user","content":"hi"}]}
    POST https://opencode.ai/zen/v1/messages             {"model":"deepseek-v4.1-flash","max_tokens":16,"messages":[...]}

结果：

    401 {"type":"error","error":{"type":"ModelError","message":"Model deepseek-v4.1-flash is not supported for format openai"}}
    401 {"type":"error","error":{"type":"AuthError","message":"Missing API key."}}
    401 {"type":"error","error":{"type":"ModelError","message":"Model deepseek-v4.1-flash is not supported for format anthropic"}}

要点：Zen（非 Go）路径下，DeepSeek 只在 chat/completions 被接受；
ModelError 与 AuthError 的区别就是「格式是否被接受」的判据。

## 2. Go 路径（带 key）

### 2.1 10:28–10:34 — 当时可用

    POST https://opencode.ai/zen/go/v1/chat/completions
    headers: Authorization: Bearer <key>, Content-Type: application/json, x-opencode-session: <uuid>
    body:    {"model":"deepseek-v4.1-flash","messages":[{"role":"user","content":"hi"}],"max_tokens":16}

    200 {"id":"ab4c7885-…","object":"chat.completion","model":"deepseek-v4.1-flash",
         "choices":[{"finish_reason":"length","message":{"role":"assistant","content":"",
         "reasoning_content":"Hmm, the u…"}}]}

（同样的请求分别用 x-opencode-session、session_id、两者同时发，三次都是 200，
说明当时 Go 订阅覆盖了该模型与格式。）

### 2.2 缺少会话头

    POST https://opencode.ai/zen/go/v1/chat/completions （无 x-opencode-session）

    400 {"type":"error","error":{"type":"MissingSessionID",
         "message":"Error from provider (Console Go): Request is missing x-opencode-session
         and cannot be routed efficiently. Please see https://opencode.ai/docs/go/#where-can-i-use-it"}}

### 2.3 11:0x — 同一 key 全部变成账务错误

    POST https://opencode.ai/zen/go/v1/chat/completions
      plain-nostream / plain-stream / tools-nostream / tools-stream  → 401 CreditsError
      glm-5.3-flash / kimi-k2.7-code / minimax-m3 / qwen3.8-flash    → 401 CreditsError

    POST https://opencode.ai/zen/go/v1/responses
      deepseek-v4-flash（有/无会话头）                                → 401 CreditsError
      deepseek-v4.1-flash                                             → 401 CreditsError
      gpt-5.6-luna                                                    → 401 CreditsError
      totally-bogus-model-xyz                                         → 401 ModelError "Model ... is not supported"

统一响应体：

    401 {"type":"error","error":{"type":"CreditsError",
         "message":"Insufficient balance. Manage your billing here:
         https://opencode.ai/workspace/wrk_01M3…/billing"}}

要点：

- 乱模型名 → ModelError；真实模型 → CreditsError，说明**模型/格式已通过，卡在账务**。
- 连 10:28 还能 200 的极简请求现在也 CreditsError → 账户状态变化，不是请求形状。

## 3. 官方文档里与本次结论相关的两段

端点表（Go）：

    Grok 4.7 / 4.6、GPT 5.6 Luna                  → https://opencode.ai/zen/go/v1/responses
    GLM-*、Kimi-*、DeepSeek V4*、MiniMax、MiMo、Qwen → https://opencode.ai/zen/go/v1/chat/completions

额度与回退：

    Usage beyond limits — If you also have credits on your Zen balance, you can enable the
    Use balance option in the console. When enabled, Go will fall back to your Zen balance
    after you have reached your usage limits instead of blocking requests.

## 4. profile 读写命令（本次用过）

    & $m --cli providers-list
    & $m --cli provider-get  --input '{"id":"relay-muc0n4hz"}'
    & $m --cli provider-update --input '{"id":"relay-muc0n4hz","patch":{"upstreamBaseUrl":"https://opencode.ai/zen/go/v1","protocol":"responses"}}'

注意：--include-secrets 只用于本地读取，输出不要贴到公开日志或提交里。
