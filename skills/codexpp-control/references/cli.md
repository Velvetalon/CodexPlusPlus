# CLI / MCP

## 原生调用

```text
codex-plus-plus-manager.exe --cli COMMAND [--input JSON|@file|-] [--dry-run] [--include-secrets]
  [--state-dir PATH --codex-home PATH]
```

| 命令 | 输入示例 |
|---|---|
| status / settings-get / providers-list | `{}` |
| settings-set | `{"patch":{"relayTestModel":"gpt-6-astra"}}` |
| provider-get / provider-switch | `{"id":"relay-a"}` |
| provider-update | `{"id":"relay-a","patch":{"name":"主力","modelList":"gpt-6-astra\ngpt-5.6-sol"}}` |
| model-routes-list | `{"id":"relay-a"}` |
| model-route-set | `{"id":"relay-a","model":"gpt-5.6-terra","enabled":false}` |
| providers-reorder | `{"ids":["relay-b","relay-a"]}` |
| aggregate-get / aggregate-switch | `{"id":"agg"}` |
| aggregate-update | `{"id":"agg","patch":{"strategy":"priorityFallback","members":[{"relayId":"relay-b","weight":1},{"relayId":"relay-a","weight":1}]}}` |
| manager-open | `{}` |
| codex-start / codex-restart | `{"debugPort":9229,"helperPort":57321,"syncActiveRelay":true}` |

provider-update 支持已有 RelayProfile 字段，包括 configContents/authContents、baseUrl/apiKey/model、contextWindow/autoCompactLimit/newContextManagement、modelWindows/modelVlm 等。不能改变 id 或 relayMode；本版管理已有供应商，不提供新建/删除供应商命令。

model-route-set 只操作已有路由。`enabled:true` 立即启用并清除恢复时间；`enabled:false` 默认 5 小时后恢复。也可三选一传 `restoreAt`（未来 UTC Unix 毫秒）、`durationSeconds`（正整数）或 `permanent:true`。操作只改 settings.json 中该路由的状态，新请求即时读取，不写 config/auth、不重启 Codex。

聚合策略允许：`failover`、`priorityFallback`、`conversationRoundRobin`、`requestRoundRobin`、`weightedRoundRobin`。
aggregate-update 的 sessionProvider 为 custom/openai；newContextManagement、模型列表/窗口位于同 ID 的 provider profile，用 provider-update。

读取时密钥和完整配置文本脱敏；--include-secrets 是仅供本地显式读取的选项，不暴露为默认 MCP 工具。CLI 修改按最新落盘配置打补丁，未指定字段保留。设置总开关关闭时允许保存配置，但不执行 live apply；切换命令明确返回失败。

CLI 的 settings/status 是落盘视图，不保证正在执行的旧会话已重新加载全部设置。普通修改不重启；启动/重启返回 accepted 后须查就绪状态，不能当完成。

## MCP stdio 注册

服务器为 Python 标准库单文件，无需 pip；默认调用 Skill 内自带的 manager。可直接使用同目录的 `mcp-registration.json`，或按以下形式加入另一个 AI 客户端的 MCP 配置：

```json
{
  "mcpServers": {
    "codexpp-control": {
      "command": "python",
      "args": ["C:/实际路径/codexpp-control/scripts/control.py", "--mcp"]
    }
  }
}
```

暴露 16 个 `codexpp_*` 工具，名字由上表命令的连字符替换为下划线。工具输入与 CLI 一致；写操作附加 `dryRun: true` 可预览。MCP stdout 仅输出 JSON-RPC，调用失败通过 isError 返回。

另一个 AI 不支持 MCP 时，可直接执行 Python/原生 CLI。无需 CDP、GUI 自动化或新的 HTTP 端口即可完成核心管理操作。

## 兼容旧版 HTTP helper

57321 提供 `/backend/status`、`/relay-rotation/status`、`/relay-rotation/reset` 和模型 API 转发；设置/完整切换并不在该 HTTP 路由表内。新功能由 manager --cli 提供。
57513 是本机 launcher guard，不是 API。普通 Codex 启动走 companion launcher，不直接运行 Codex.exe，以保证注入步骤。
