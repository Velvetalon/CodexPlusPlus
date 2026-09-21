# Responses history sanitization - 2026-09-21

## Symptom

Two real turns failed instantly (410 ms / under 1 s) with upstream 400s while the
thread history was replayed to a strict Responses endpoint:

1. "Duplicate tool output for call_id: call_c030b640765845598e4e5dcd."
2. "Invalid 'input[356].id': 'chatcmpl-202609200918258761407968268d9d6WGd5KQKm_msg_0'.
   Expected an ID that begins with 'msg'."

Both requests were rejected before the model produced a single token, so the user
saw a conversation that could no longer continue.

## Root cause

**Duplicate tool outputs.** The Codex client can record more than one
custom_tool_call_output response item for a single tool call when the streamed
exec output is split. Evidence: session
rollout-2026-09-21T09-49-04-01a0c1a7-...jsonl, ordinals 1315 and 1317 both carry
call_id = call_c030b640765845598e4e5dcd - the first with the wrapper text
"Script completed ... Output:" and the second with "5610". The normal shape for
that output is one item whose output is an array of input_text parts
(ordinal 1311). Responses and ChatCompletions upstreams accept exactly one result
per tool call, so the replay is invalid.

**Foreign item ids.** Item ids produced by a relay channel (chatcmpl-...) were
replayed unchanged to strict upstreams. normalize_responses_item_ids_with_policy
only rewrote ids under the official-DeepSeek profile, so every other Compatible
Responses upstream (including the OpenAI-official bridge at 127.0.0.1:8787) kept
the foreign namespace and failed validation.

## Change

crates/codex-plus-core/src/protocol_proxy.rs

- merge_duplicate_tool_outputs(&mut request_json) runs at the top of
  upstream_request_parts, before the Responses/ChatCompletions split, so both
  wire paths are protected. Items of type function_call_output /
  custom_tool_call_output with a non-empty call_id are grouped; the first
  occurrence keeps its position, type, id and flags, later siblings are merged
  into it and dropped. Merge order is encounter order: a string becomes one
  input_text part, an array keeps its parts, other values become compact JSON
  text, null contributes nothing. When the kept item's output was a plain
  string and exactly one text part results, the plain string shape is preserved.
  A missing name is filled from a sibling. With no duplicates the body is left
  untouched. This runs regardless of wire policy: duplicate outputs are invalid
  input, not a structure preference.
- normalize_responses_item_ids_with_policy(&mut body, true) is now used for
  every Compatible Responses upstream, so message / reasoning / function-call /
  function-output ids are rewritten into the msg_ / rs_ / fc_ / fco_ namespaces
  instead of being forwarded with a foreign prefix.

## Verification

    cargo test -p codex-plus-core --test protocol_proxy   104 passed, 0 failed, 1 ignored
    cargo test -p codex-plus-core --lib                   380 passed, 0 failed

New unit coverage: duplicate merge keeps both texts and first position, three-way
duplicates concatenate once, array + string order is preserved, plain-string shape
is preserved for a single part, non-string scalars become compact JSON text,
distinct call ids and non-output items are untouched, and non-array/missing input
is unchanged. New socket-level tests assert that a request with two
custom_tool_call_output items reaches the upstream as one item with the sibling
name filled in, and that a Compatible relay now rewrites chatcmpl-*.id history
into the msg_ / fc_ / fco_ namespaces.

## Build

    dist/windows/app/codex-plus-plus.exe          18479104 bytes  sha256 90EFE0982BB2006DF40B878D62DD010203C1C776442D76267BB80D52E935AC6E
    dist/windows/app/codex-plus-plus-manager.exe  38407168 bytes  sha256 89AB3FEA78F5EDA0EBEA7FA366C64F1EFBDCBAC26BE4049087883B6E092069B5

The live proxy must be restarted to pick the binaries up; a running instance keeps
the image it was started with.
