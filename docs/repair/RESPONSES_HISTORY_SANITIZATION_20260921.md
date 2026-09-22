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

## Follow-up: encrypted reasoning retry (same day)

Symptom: a thread became permanently unusable with an upstream 400
"The encrypted content for item rs_905140f8752c46cfa4049cf31f7d9768 could not be
verified. Reason: Encrypted content could not be decrypted or parsed."
(code invalid_encrypted_content). The relay serving that thread is the OpenAI
account bridge (relay "OpenAI账号中转", http://127.0.0.1:8787/v1/responses), whose
reasoning policy keeps items carrying encrypted_content and drops the rest, so the
unverifiable blob was replayed on every attempt and the turn died before any token
was produced.

Not a regression of the sanitizer build: the diagnostic log
(<codex-home>/.codex-session-delete/codex-plus.log) shows the first rejection at
11:38:58Z served by pid 59568 - the previous proxy image - while the rebuilt proxy
(pid 50864) only started at 11:40:38Z. The rejected reasoning id already existed
verbatim in the rollout (recorded 09:29:08Z), so id rewriting never touched it, and
encrypted_content is not modified by the proxy.

Root cause: encrypted reasoning blobs are only portable inside the account that
issued them. When the account behind an account-pool relay rotates (or the blob
ages out), replaying that history is rejected wholesale.

Change: open_responses_proxy_request_with_settings_and_user_agent now buffers a 400
response body when the request carried encrypted reasoning items, and if the body
reports an encrypted-content verification failure (invalid_encrypted_content, or
"encrypted content" together with could not be / decrypt / parsed) it retries the
same relay once with every reasoning item that carries a non-empty
encrypted_content removed. Other 400s are returned untouched through a rebuilt
response (same status, content-type and body bytes). The retry happens at most once
per relay attempt, is logged as protocol_proxy.encrypted_reasoning_retry, and does
not mutate the caller's request JSON.

Verification: cargo test -p codex-plus-core --test protocol_proxy => 107 passed,
0 failed, 1 ignored; cargo test -p codex-plus-core --lib => 380 passed. New socket
tests cover the successful retry (two upstream requests, second without encrypted
reasoning, other items byte-identical), the untouched-error path (exactly one
upstream request, original 400 body and content-type), and the "request has no
encrypted reasoning" guard.

Build: dist/windows/app/codex-plus-plus.exe sha256
E2E58B1F2DFE5050A0854E420B774B094C04E43EA680C750742371C2DC8338E0,
dist/windows/app/codex-plus-plus-manager.exe sha256
D811E4D1463E63AEF79AFABA35A9833789A35BBABFA6901A976DF2A5D604DD5B.

## Follow-up: multi-agent tool name restore (same day)

Symptom: sub-agent tools intermittently failed with
"unsupported call: collaboration__spawn_agent", raised by
codex_core::tools::router in the Codex core, so the whole sub-agent workflow
stalled in that thread.

Root cause: these tools are registered client-side as a namespace tool
(namespace collaboration, tool spawn_agent). The Compatible wire path flattens
namespace tools into a single flat function name (collaboration__spawn_agent) for
upstreams that cannot express namespaces, and the response path has to restore the
flat name before the client sees it. Restore only matched an exact key of the
flattened-name map built from that request, so any other spelling the model chose
(codexpp_native_collaboration__spawn_agent, or the flat name in a turn whose
request carried no namespace map at all) leaked through unrepaired.

Evidence: session rollout of 2026-09-21 records successful calls as
function_call { namespace: collaboration, name: spawn_agent } while the failures
are function_call { name: collaboration__spawn_agent } with no namespace; the
desktop log records 33 plain spawn_agent dispatches (all fine) against 6
collaboration__spawn_agent dispatches that all ended in the router error.

Change: restore_responses_tool_namespace_item now falls back to a prefix-agnostic
repair when the exact map lookup misses. A call name shaped <prefix>__<tool>,
where the prefix is collaboration or ends with collaboration and the tool is one
of spawn_agent / wait_agent / send_message / followup_task / list_agents /
interrupt_agent, is rewritten to the registered namespace plus bare tool name
(namespace taken from the request map when known, otherwise collaboration). Other
namespaces and other tools are never touched. The empty-map fast path stays in
place: restore only parses when the payload actually contains a flat multi-agent
tool suffix.

Verification: cargo test -p codex-plus-core --test protocol_proxy => 110 passed,
0 failed, 1 ignored; cargo test -p codex-plus-core --lib => 380 passed. New tests
cover both prefixed spellings with an empty map, that unrelated flat names
(functions__exec, other_namespace__spawn_agent) stay untouched, that exact map hits
remain authoritative, and that the SSE frame rewriter performs the same repair.

Build: dist/windows/app/codex-plus-plus.exe sha256
0D2EF44A3C091941EE56B54CF10CE8D3B2818E70ABCA88ACE7F40FD038C619A6,
dist/windows/app/codex-plus-plus-manager.exe sha256
F6540F5E9B00DB5DFF3512F134B0FA37E35FD74147D0D9DBDBCBA5C62B3F4212.

## Follow-up: tool outputs without call_id (2026-09-22)

Symptom: thread 01a0c78b died with 422 from the DeepSeek Responses endpoint -
"Failed to deserialize the JSON body into the target type: input: missing field
call_id at line 1 column 671252" - and every retry failed the same way.

Root cause: the inter-agent message delivery path writes a function_call_output
item without call_id into the client history. In the affected rollout the item
appears right after a subagent message was delivered (item id fco_01a0c795-...,
name send_message_to_thread, no call_id), and the strict DeepSeek endpoint rejects
the whole request. Reproduced directly against api.deepseek.com/v1/responses: a
body containing {"type":"function_call_output","name":"send_message_to_thread",
"output":"hello"} without call_id returns 422 with exactly that message, while the
same body with call_id passes that validation. The ChatCompletions converter
already dropped such items; only the Responses path forwarded them verbatim.

Change: drop_tool_outputs_without_call_id runs next to the duplicate-output
merge in upstream_request_parts, before the Responses/ChatCompletions split, and
removes function_call_output / custom_tool_call_output items whose call_id is
missing or empty. Everything else in the history is untouched, and the payload is
not re-serialized when no such item exists.

Verification: cargo test -p codex-plus-core --test protocol_proxy => 111 passed,
0 failed, 1 ignored; cargo test -p codex-plus-core --lib => 381 passed. New tests
cover the drop helper (both item types, keeps valid outputs and messages) and a
socket-level request that must reach the upstream without the orphan item.

Build: dist/windows/app/codex-plus-plus.exe sha256
20492411241768D42E762ACD4F454310FD4A22AC6599566CF809DAEA55DC609C,
dist/windows/app/codex-plus-plus-manager.exe sha256
FA6007D24DDC0FDA617CDAD4ECB6F7692D2B05857C02FC02F45429AE5FA58AF5.
