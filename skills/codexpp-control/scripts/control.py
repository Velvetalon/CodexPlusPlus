"""Native Codex++ CLI adapter and newline-delimited stdio MCP server (stdlib only)."""
import argparse
import json
import pathlib
import subprocess
import sys

READ = {"status", "settings-get", "providers-list", "provider-get", "aggregate-get"}
PATCH = {"type": "object", "description": "Only fields to change; omitted fields remain unchanged."}
ID = {"type": "string", "minLength": 1, "description": "Exact saved provider ID, from providers-list."}
DESCRIPTIONS = {
    "status": "Read Codex++ config/active provider and recorded launch status. Does not launch apps.",
    "settings-get": "Read saved settings; credentials and configuration text are redacted.",
    "settings-set": "Patch general settings and apply live provider config if enabled. Use specific provider tools for profiles/selection.",
    "providers-list": "List saved providers, aggregate members, strategy and current selection, in priority order.",
    "provider-get": "Read one saved provider and whether it is active.",
    "provider-update": "Patch an existing provider. Active provider or active aggregate member changes are applied to live config without restart. Supports configContents/authContents, modelList/contextWindow/newContextManagement, baseUrl/apiKey/model.",
    "provider-switch": "Select saved ordinary or aggregate provider; perform real config/auth/catalog apply using the same switch service as the GUI. Does not restart Codex.",
    "providers-reorder": "Move listed IDs to the front in order; unlisted providers keep relative order. Selected aggregate members follow this global order; does not restart Codex.",
    "aggregate-get": "Read one aggregate's strategy/members and corresponding provider settings.",
    "aggregate-update": "Patch an existing aggregate: name, strategy, members, sessionProvider or codeModeHost. Use providers-reorder for member priority. Applies active config without restart.",
    "aggregate-switch": "Select a saved aggregate and apply its live configuration without restarting Codex.",
    "manager-open": "Open or focus the manager GUI. This is not needed for other tools.",
    "codex-start": "Start/reuse Codex through its companion Codex++ launcher, including injection. Returns accepted; verify readiness separately.",
    "codex-restart": "Restart Codex++ and Codex via manager restart workflow. INTERRUPTS active tasks; invoke only when the user requests a restart. Returns accepted, not proof of readiness.",
}
STRATEGIES = ["failover", "priorityFallback", "conversationRoundRobin", "requestRoundRobin", "weightedRoundRobin"]


def schema(command):
    fields, required = {}, []
    if command in {"provider-get", "provider-update", "provider-switch", "aggregate-get", "aggregate-update", "aggregate-switch"}:
        fields["id"], required = ID, ["id"]
    if command.endswith("-update") or command == "settings-set":
        fields["patch"] = PATCH
        required.append("patch")
    if command == "aggregate-update":
        fields["patch"] = {"type": "object", "additionalProperties": False, "properties": {
            "name": {"type": "string"}, "strategy": {"type": "string", "enum": STRATEGIES},
            "sessionProvider": {"type": "string", "enum": ["custom", "openai"]},
            "codeModeHost": {"type": "boolean"},
            "members": {"type": "array", "minItems": 1, "items": {
                "type": "object", "required": ["relayId"], "additionalProperties": False,
                "properties": {"relayId": ID, "weight": {"type": "integer", "minimum": 1}},
            }},
        }}
    if command == "providers-reorder":
        fields["ids"] = {"type": "array", "items": ID, "uniqueItems": True}
        required.append("ids")
    if command in {"codex-start", "codex-restart"}:
        fields.update(appPath={"type": "string"}, syncActiveRelay={"type": "boolean", "default": True})
        for name, default in (("debugPort", 9229), ("helperPort", 57321)):
            fields[name] = {"type": "integer", "minimum": 1, "maximum": 65535, "default": default}
    if command not in READ:
        fields["dryRun"] = {"type": "boolean", "default": False, "description": "Preview only; do not write or launch anything."}
    return {"type": "object", "properties": fields, "required": required, "additionalProperties": False}


def tools():
    return [{"name": "codexpp_" + name.replace("-", "_"), "description": description,
             "inputSchema": schema(name), "annotations": {
                 "readOnlyHint": name in READ, "destructiveHint": name == "codex-restart",
                 "idempotentHint": name not in {"codex-start", "codex-restart", "manager-open"},
                 "openWorldHint": name in {"codex-start", "codex-restart"},
             }} for name, description in DESCRIPTIONS.items()]


def call(options, command, payload, dry_run=False):
    arguments = [str(options.manager), "--cli", command, "--input", "-"]
    if options.state_dir:
        arguments += ["--state-dir", options.state_dir, "--codex-home", options.codex_home]
    if dry_run:
        arguments.append("--dry-run")
    if options.include_secrets:
        arguments.append("--include-secrets")
    try:
        result = subprocess.run(arguments, input=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=options.timeout,
                                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
    except subprocess.TimeoutExpired:
        return {"status": "failed", "message": "CLI timed out. The mutation might have completed; read current state before retrying."}, 1
    except OSError as exc:
        return {"status": "failed", "message": f"Cannot start manager CLI: {exc}"}, 1
    try:
        output = json.loads(result.stdout.decode("utf-8-sig"))
    except (ValueError, UnicodeError):
        return {"status": "failed", "message": "Manager returned no CLI JSON. Use the new CLI-enabled build, not an older GUI-only binary.", "exitCode": result.returncode}, 1
    return output, result.returncode


def write(message):
    sys.stdout.buffer.write((json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8"))
    sys.stdout.buffer.flush()


def serve(options):
    initialized = False
    supported = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
    names = {"codexpp_" + name.replace("-", "_"): name for name in DESCRIPTIONS}
    for line in sys.stdin.buffer:
        if not line.strip():
            continue
        try:
            message = json.loads(line)
        except ValueError:
            write({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "Parse error"}})
            continue
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0" or not isinstance(message.get("method"), str):
            write({"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": "Invalid request"}})
            continue
        if "id" not in message:  # Notifications have no response.
            continue
        reply = {"jsonrpc": "2.0", "id": message["id"]}
        method, params = message["method"], message.get("params", {})
        if not isinstance(params, dict):
            reply["error"] = {"code": -32602, "message": "params must be an object"}
        elif method == "initialize":
            initialized = True
            version = params.get("protocolVersion")
            reply["result"] = {"protocolVersion": version if version in supported else supported[0],
                               "capabilities": {"tools": {}}, "serverInfo": {"name": "codexpp-control", "version": "2.0.0"}}
        elif method == "ping":
            reply["result"] = {}
        elif not initialized:
            reply["error"] = {"code": -32000, "message": "Initialize first"}
        elif method == "tools/list":
            reply["result"] = {"tools": tools()}
        elif method == "tools/call":
            name = params.get("name")
            command = names.get(name) if isinstance(name, str) else None
            data = params.get("arguments", {})
            if not command or not isinstance(data, dict):
                reply["error"] = {"code": -32602, "message": "Unknown tool or invalid arguments"}
            else:
                data = dict(data)
                definition = schema(command)
                unknown = set(data) - set(definition["properties"])
                missing = set(definition["required"]) - set(data)
                if unknown or missing or ("dryRun" in data and not isinstance(data["dryRun"], bool)):
                    reply["error"] = {"code": -32602, "message": "Invalid tool arguments"}
                else:
                    dry_run = data.pop("dryRun", False)
                    output, exit_code = call(options, command, data, dry_run)
                    reply["result"] = {"content": [{"type": "text", "text": json.dumps(output, ensure_ascii=False)}],
                                       "structuredContent": output, "isError": exit_code != 0 or output.get("status") == "failed"}
        else:
            reply["error"] = {"code": -32601, "message": "Method not found"}
        write(reply)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manager", required=True, type=pathlib.Path)
    parser.add_argument("--mcp", action="store_true")
    parser.add_argument("--state-dir")
    parser.add_argument("--codex-home")
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--include-secrets", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--input", default="{}", help="JSON, @file.json or - for stdin")
    parser.add_argument("command", nargs="?", choices=["help"] + list(DESCRIPTIONS))
    options = parser.parse_args()
    if not options.manager.is_file():
        parser.error("Manager executable not found")
    if bool(options.state_dir) != bool(options.codex_home):
        parser.error("Supply both --state-dir and --codex-home")
    if options.mcp:
        if options.include_secrets:
            parser.error("--include-secrets is for explicit CLI exports only")
        serve(options)
    else:
        raw = options.input
        if raw == "-": raw = sys.stdin.buffer.read().decode("utf-8-sig")
        elif raw.startswith("@"): raw = pathlib.Path(raw[1:]).read_text(encoding="utf-8-sig")
        output, code = call(options, options.command or "help", json.loads(raw), options.dry_run)
        write(output)
        sys.exit(code)


if __name__ == "__main__":
    main()
