# Unsolicited stop analysis - 2026-09-21

Scope: Codex Desktop thread `01a0bcff-f68e-71e0-bead-99ab0cbaa1b5`, sessions of 2026-09-20. Sources: the three archived rollout JSONL files, the desktop log SQLite database `<codex-home>/logs_2.sqlite` (read-only), and `<codex-home>/config.toml`. Timestamps below are UTC (local = UTC+8).

## Symptom

The user reported two turns that appeared to stop by themselves while a DeepSeek-relayed model profile served the thread, and insisted neither was a deliberate stop. Forensic reading of the rollout files shows the two stops are different failure classes, and neither is a network drop, provider outage, or app restart.

- Stop #1 (turn `01a0bde3-ad84-7460-9c3d-45cd2a97d7a1`): the agent loop exited normally after the upstream returned a final chat-only response, while the last visible sentence promised an `apply_patch` tool call that never arrived. The rollout records a clean `task_complete` with no error.
- Stop #2 (turn `01a0be17-5cd6-7730-9c65-b1ad9e2d5503`): the turn was genuinely interrupted by an explicit client `Interrupt` operation while a `wait_agent` call had been pending for 1108.9 s. The interrupt op came from the Codex Desktop renderer, not from a restart or a crash.

A user message paraphrased as the Luna-MAX sentence belongs to a different turn, `01a0be0c-91f8-7d52-8bf3-a67cc1636f5b`, which also ended in a clean `task_complete` (premature-completion class, like Stop #1). It was NOT the aborted turn. The checkpoint claim that both stops were host/UI interruptions is wrong for Stop #1 and only partially right for Stop #2: the abort was real but its turn was the successor turn, not the paraphrased one.

## Evidence per stop

### Stop #1 - turn 01a0bde3 (premature clean completion)

Identity and window

- Turn id `01a0bde3-ad84-7460-9c3d-45cd2a97d7a1`, rollout `rollout-2026-09-20T15-39-47-..._01a0bdc1-....jsonl`, ordinals 1367-1554.
- Started 2026-09-20T08:16:50Z (16:16:50 local), completed 08:38:07Z (16:38:07 local), duration 1277.4 s, time-to-first-token 9576 ms.
- Turn cumulative usage at completion: input 7,590,786 / cached 3,129,425; output 19,051 of which reasoning 9,531.
- Final upstream response (`chatcmpl-202609200837540092734028268d9d6hVLJvj8c`) usage: input 289,733 / cached 286,720; output 36 of which reasoning 1.

Ordered item highlights (24 upstream responses in the turn)

1. Ordinal 1372: user message about switching model and continuing.
2. Ordinals 1375-1550: 24 upstream responses, each producing agent messages plus reasoning plus tool calls. The turn emitted 31 `function_call` items in total; only 17 `function_call_output` items exist.
3. Of the 31 calls, 14 were `apply_patch` (ordinals 1421, 1426, 1431, 1450, 1455, 1460, 1472, 1477, 1482, 1494, 1497, 1504, 1517, 1538). None of the 14 produced a `function_call_output` anywhere in the rollout file, even after string search for each `call_id`.
4. The turn commentary repeatedly acknowledged the missing outputs (ordinals 1501 and 1542 both say the patch interface kept getting aborted). Ordinal 1511 shows the one recorded failure payload for an exec_command fallback attempt: `"out":"exec_command failed: CreateProcess { message: Rejected(...`.
5. Ordinal 1551 is the final assistant message, promising a tool call that is never recorded:

```json
{"timestamp":"2026-09-20T08:38:07.467Z","ordinal":1551,"type":"response_item","payload":{"type":"message","id":"chatcmpl-202609200837540092734028268d9d6hVLJvj8c_msg_0","role":"assistant","content":[{"type":"output_text","text":"本机 `apply_patch` CLI 可用，我先用它写入 T00 证据文件（内容较长，通过标准输入传入），成功后再更新任务图。"}],"internal_chat_message_metadata_passthrough":{"turn_id":"01a0bde3-ad84-7460-9c3d-45cd2a97d7a1","content_item_kinds":["unknown"]}}}
```

6. Termination - clean completion, no error, no tool call in the final response:

```json
{"timestamp":"2026-09-20T08:38:07.504Z","ordinal":1554,"type":"event_msg","payload":{"type":"task_complete","turn_id":"01a0bde3-ad84-7460-9c3d-45cd2a97d7a1","last_agent_message":"本机 `apply_patch` CLI 可用，我先用它写入 T00 证据文件（内容较长，通过标准输入传入），成功后再更新任务图。","started_at":1789892210,"completed_at":1789893487,"duration_ms":1277439,"time_to_first_token_ms":9576}}
```

7. Token-count event immediately before completion, ordinal 1553, `last_token_usage.output_tokens = 36`, `reasoning_output_tokens = 1`, no rate-limit flags.

Interpretation: the model itself ended the response with a text-only finish after emitting a chat sentence; the client then closed the turn because the upstream stream genuinely reported completion. The missing 14 apply_patch outputs show the relay adapted tool-call traffic badly across this turn, and the final 36-token text-only response is the classic signature of a relay that dropped the trailing function_call item and forced an early response.completed. This is inference, clearly separated below from the raw facts.

### Stop #2 - turn 01a0be17 (client interrupt while wait_agent pending)

Identity and window

- Turn id `01a0be17-5cd6-7730-9c65-b1ad9e2d5503`, rollout `rollout-2026-09-20T17-13-16-..._01a0be17-....jsonl`, ordinals 1771-1915.
- Started 2026-09-20T09:13:17Z (17:13:17 local), aborted 09:39:54Z (17:39:54 local), duration 1597.0 s.
- User prompt at ordinal 1773: "继续。我刚才单独拉起luna对话确认luna是可用的".
- Inside the turn the agent spawned a Luna subagent (`t01_residency_fix`, ordinal 1779), then a probe (`luna_probe`, ordinal 1816), then a Terra executor (`terra_executor`, ordinal 1893). It entered `wait_agent` at ordinal 1910 at 09:21:25Z with `timeout_ms: 1500000`.

Key raw lines

Tool output for the pending wait (ordinal 1912, rollout3 line 885):

```json
{"timestamp":"2026-09-20T09:39:54.279Z","ordinal":1912,"type":"response_item","payload":{"type":"function_call_output","name":"wait_agent","output":"aborted by user after 1108.9s","call_id":"..."}}
```

Turn boundary:

```json
{"timestamp":"2026-09-20T09:39:54.297Z","ordinal":1915,"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"01a0be17-5cd6-7730-9c65-b1ad9e2d5503","reason":"interrupted","started_at":1789895597,"completed_at":1789897194,"duration_ms":1597028}}
```

Desktop log correlation (from `logs_2.sqlite`)

- ts 1789897194 (09:39:54Z / 17:39:54 local), thread `01a0be1e-65e9-75b3-8163-127db740b8a3` (the luna_probe subagent session):

```
DEBUG codex_core::session::handlers  ... Submission { id: "01a0be2f-bc44-75e0-9f3e-d98f75bfbd3f", op: Interrupt, ... }
INFO  codex_core::session           ... op.dispatch.interrupt ...: interrupt received: abort current task, if any
TRACE codex_core::tasks             ... aborting running task task_kind=Regular sub_id="01a0be1e-661e-..."
```

- ts 1789897199 (09:39:59Z / 17:39:59 local), thread `01a0be25-ea2d-7960-884e-5b6c3dc5a284` (the terra_executor subagent session): the same three-line Interrupt sequence with submission id `01a0be2f-cdb3-7212-9e9d-a10359fdc4e7`.
- The next user turn (`01a0be31`, user text about updating and pushing to delay) started only at 09:41:22Z, i.e. 88 s after the abort. No thread restart, no session_meta re-init, no queued user message between the interrupt and the turn boundary. The interrupt therefore did not come from a queued follow-up or an app restart.
- During the same 09:39:40-09:40:20 window the log also recorded about 1800 `ERROR codex_core::util ... ReasoningSummaryDelta without active item` entries against the two subagent turn ids, model `gpt-5.6-terra`, effort `high`/`max`. These are relay-stream protocol errors, not the interrupt trigger (they had been repeating since about 09:36:40Z), but they prove the relay was streaming malformed reasoning deltas to the subagent loops while the main thread was waiting.

What the interrupt was

The `Interrupt` op is the standard client-side stop path: the desktop renderer sends it to the core session when the user clicks Stop, presses Escape, or issues the equivalent command. The rollout itself writes `aborted by user after 1108.9s` into the tool output. The timestamps rule out an app restart and a queued user message. The prior checkpoint's host-initiated theory is not supported: the record shows a user-level interrupt operation, and no other mechanism matches the log signature.

### The paraphrase turn - 01a0be0c (mis-attributed, also a premature completion)

- Turn id `01a0be0c-91f8-7d52-8bf3-a67cc1636f5b`, same second rollout, ordinals 1679-1765.
- Started 09:01:30Z, task_complete at 09:10:56Z, ttft 278,518 ms (4 min 38 s), duration 566 s.
- Final assistant message (ordinal 1760) is exactly the sentence the user quoted as having been cut off. Termination is a clean `task_complete` (ordinal 1765), with no error and no tool call inside the final upstream response (`chatcmpl-202609200909251867653218268d9d6uaYh3aZI`).
- The turn itself did run real tool work (a `collaboration__spawn_agent` attempt that failed with `unsupported call`, a working `spawn_agent`, `wait_agent`, and several `exec_command`s), so unlike Stop #1 it is not a pure no-op finish; but its final response is again chat-only while announcing further Luna delegation.

## Correlation

- Channel: `config.toml` routes the client to `http://127.0.0.1:57321/v1` (Codex++ local relay), with the Codex++ upstream set to `http://127.0.0.1:8787/v1`, `wire_api = "responses"`, catalog `model-catalogs/relay-mt9rmsjy.json`. Every turn in the investigated thread used `model_provider_id = "custom"`. The specific DeepSeek relay identity behind port 8787 at 2026-09-20 16:xx-17:xx local is not recorded in the surviving logs; only the Codex++ port topology is proven.
- Stop #1 (08:16-08:38Z) has zero desktop-log records inside the turn window; the SQLite log retention for this thread only starts at 1789897562, so no desktop-side stream diagnosis for Stop #1 survives. The absence of any `op.dispatch.interrupt` in the rollout itself, combined with the clean `task_complete`, is the strongest remaining evidence.
- Stop #2 (09:13-09:39Z) is fully covered by desktop logs: the client Interrupt op at 1789897194/1789897199 and the 1800 ReasoningSummaryDelta stream errors are both on record. The interrupt arrives 88 s before the next user message and kills the main turn plus two subagent sessions within 5 s of each other.

## Verdict

1. Stop #1 (`01a0bde3`): PREMATURE CLEAN COMPLETION, confidence high. Proven: the upstream's last response contained only a 36-token chat message and no tool call, and the client closed the turn with `task_complete` and no error. Inferred (medium-high): the DeepSeek relay dropped the trailing tool-call item that the model's visible text was announcing, consistent with the 14 silently-vanished apply_patch outputs in the same turn. This was NOT a user or host interruption.
2. Stop #2 (`01a0be17`): USER-LEVEL CLIENT INTERRUPT, confidence high. Proven: `op: Interrupt` submissions in the desktop log, `aborted by user after 1108.9s` in the rollout, and no restart/queue signature. The interrupt ended the main turn plus the two running subagent sessions. NOT host-initiated, NOT a crash, NOT a model switch.
3. The user-quoted sentence belongs to turn `01a0be0c`, which ALSO ended in a premature clean `task_complete` (same class as Stop #1), confidence high. The prior checkpoint's claim that this turn aborted on a pending wait_agent is refuted for that specific turn; the actual aborted turn was its successor.

## What Codex++ can and cannot fix

Provable targets

1. Silent tool-output loss: 14 `apply_patch` calls in Stop #1 produced no `function_call_output` at all. Codex++ can detect an executed tool call whose response item never arrived and synthesize a timeout/error output so the model sees the failure instead of guessing.
2. Early-completion detection: the final response of Stop #1 ended text-only right after the model announced a tool call. Codex++ can flag a `response.completed` that lands on chat-only text containing tool-intent markers and retry the request once with the history intact.
3. Malformed reasoning deltas: the 1800 `ReasoningSummaryDelta without active item` errors in the Stop #2 window show the relay emitting reasoning events outside a proper item envelope. Codex++ can drop or re-anchor those deltas instead of passing them through as per-item ERROR logs.
4. Interrupt path is a client feature, not a bug: the Stop #2 abort came through the legitimate `op.dispatch.interrupt` path. Codex++ cannot and should not block it. What it can do is expose in the UI which sessions an interrupt will cascade to (main turn plus subagents), since today the user may not realize that a stop also kills `wait_agent` and spawned agents.

Cannot be fixed by Codex++

- The DeepSeek relay's decision to close a response after a chat-only segment. Codex++ sits between the client and the relay and can retry or surface the anomaly, but the relay's own finish semantics are upstream.
- Log retention: the desktop SQLite log has no records before 1789897562 for this thread, so any stream-level evidence for Stop #1 outside the rollout file is unrecoverable.

## Open questions

- Which concrete DeepSeek relay (name/endpoint) served port 8787 on 2026-09-20 is not recorded anywhere we can read; only the Codex++ topology is proven.
- Whether the relay drops tool-call items because of request-size limits (the Stop #1 patch bodies were large) or because of the Custom-as-Function adapter path; the surviving data cannot distinguish these.
- Whether the user pressed Stop deliberately at 09:39:54 or the desktop UI auto-issued the interrupt (for example through a stop-button interaction while switching panes); the log records the op source as the desktop client but not the concrete UI gesture. The rollout's own `aborted by user` wording is the client's label, not proof of a deliberate keystroke.
