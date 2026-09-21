# Official DeepSeek Responses Compatibility - 2026-09-21

## Symptom

Official DeepSeek Responses sessions could tell the user that the execution tool was unavailable and ask for apply_patch, while third-party DeepSeek-compatible relays worked. Long threads also failed on Responses history validation with IDs such as chatcmpl-..._msg_0 because the item type expected a msg_ prefix.

## Root Cause

The official DeepSeek compatibility path disabled Code Mode but did not provide an execution tool path that the official endpoint accepted. Its catalog also left tool_mode unset. Separately, foreign Responses history IDs were only rewritten when they began with item_, so relay-generated chatcmpl message IDs were replayed unchanged.

## Change

Official DeepSeek profiles keep Code Mode enabled, force unified_exec, and mark generated DeepSeek catalogs as code_mode_only. The request proxy force-enables the existing Custom-as-Function adapter for official DeepSeek Responses, including Compatible rewrites even when a stale profile selected passthrough. The client still sees custom tools; DeepSeek receives function declarations and function-call history.

Official DeepSeek strict-prefix normalization maps foreign message IDs to msg_, reasoning IDs to rs_, function calls to fc_, and function outputs to fco_. Third-party relays retain the existing conservative normalization.

## Verification

Protocol proxy: 102 passed, 1 ignored. Relay config: 144 passed. Core lib: 372 passed. Launcher: 85 passed. Relay rotation: 26 passed. Rust formatting and git diff checks passed on the modified Rust files.

The official DeepSeek test captures an upstream request through a mock Responses endpoint and verifies both custom-tool wrapping and msg_ history normalization without exposing provider credentials.
