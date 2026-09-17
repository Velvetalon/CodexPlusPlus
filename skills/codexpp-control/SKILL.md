---
name: codexpp-control
description: 通过原生 manager CLI 或 stdio MCP 管理 Codex++，无需 GUI 点击。用于读取/修改设置，切换普通与聚合供应商、调整供应商顺序及聚合策略，打开 manager，以及通过 launcher 启动注入后的 Codex 或显式重启。要求使用带 --cli 的新版 manager。
---

# Codex++ 原生管理

直接调用新版 `codex-plus-plus-manager.exe --cli <命令>`，stdout 为一个 UTF-8 JSON，退出码 0 成功、1 失败。不需要先开 manager。使用和新版 manager 同目录的配套 launcher；旧稳定版不识别 --cli，不要拿旧 exe 试命令。

默认使用 Skill 内 `bin/codex-plus-plus-manager.exe`。用户指定其他安装目录时可传 `--manager` 覆盖，先确认是新版 CLI。

先读 `--cli help` 和 `--cli providers-list`。所有选取使用保存的 id，不用名称或猜测索引。配置输入用 `--input @file.json`，或者 `--input -` 从 stdin 读取 JSON；密钥不要放到命令行。

```powershell
$manager = Join-Path $PSScriptRoot 'bin\codex-plus-plus-manager.exe'
& $manager --cli providers-list
& $manager --cli provider-switch --input '{"id":"已保存供应商ID"}'
& $manager --cli aggregate-update --input '{"id":"聚合ID","patch":{"strategy":"priorityFallback"}}'
& $manager --cli providers-reorder --input '{"ids":["第一优先ID","第二优先ID"]}'
```

需要稳定捕获 Windows GUI exe 的 JSON 时，用本目录 Python 封装：

```text
python scripts/control.py providers-list
python scripts/control.py --input @change.json provider-update
python scripts/control.py --dry-run codex-restart
```

读取默认隐藏凭据和完整配置文本；确需读取 TOML/auth 用显式 CLI `--include-secrets`，不要把其输出带入公开日志。读取的脱敏对象不能原样提交；更新只传真正改变的字段。

## 行为

- `settings-get/settings-set`：获取设置或以 `{patch:{...}}` 更新普通设置。供应商相关数组和当前选择走专用命令。启用供应商配置时写操作同步 live 配置。
- `provider-get/provider-update`：读/改已有供应商。更新活动供应商或活动聚合的成员会同步配置；不重启。聚合的上下文模式等字段同样通过 provider-update 设置。
- `model-routes-list/model-route-set`：读取单模型转发的有效状态、恢复时间和剩余时长，或立即启用/禁用某一项。禁用默认 5 小时，也可指定恢复时间、持续秒数或永久禁用；新请求立即生效，不重启 Codex，不中断正在执行的请求。
- `provider-switch/aggregate-switch`：真正执行 manager 使用的配置切换与回滚服务，不是只改 activeRelayId。
- `providers-reorder`：给出的 IDs 按顺序移到前面，其余保留相对顺序。聚合成员优先级继承全局供应商顺序。
- `aggregate-get/aggregate-update`：读/改聚合成员、策略、会话身份、codeModeHost。members 更新的是成员选择和权重，不另创独立顺序规则。
- `manager-open`：打开/聚焦 manager。其余命令不依赖 GUI。
- `codex-start`：复用 manager 的 launcher 启动流程，包含 Codex 注入；`accepted` 只表示启动已发出，用 status/latestLaunch 和原 helper 健康/bridge 检查确认就绪。
- `codex-restart`：复用 manager 重启流程，默认同步活动配置，确实会中断正在执行的任务。仅在用户要求重启时执行，不为普通供应商修改顺便重启。

用 `--dry-run` 预览。测试写入同时指定 `--state-dir <临时设置目录> --codex-home <临时Codex目录>`；该模式的启动/重启只允许 dry-run。写失败先检查返回结果/当前状态，不自动重试。
原生 CLI 与新版 GUI 共用文件锁。已经打开的 manager 若有未保存草稿，先重新载入设置再继续编辑，避免随后保存旧草稿覆盖 CLI 更改。

详细输入和 MCP 注册见 [cli.md](references/cli.md)。现有 HTTP/CDP 辅助接口仍可用于状态、冷却和会话导出，但不要把 Tauri command 当 HTTP 路由。
