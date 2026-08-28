import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { orderAggregateMembersByCandidates } from "./relay-aggregate-order.ts";

test("selected aggregate members follow relay profile candidate order", () => {
  const stored = [
    { profileId: "krill", weight: 2 },
    { profileId: "shuai-api", weight: 3 },
    { profileId: "openai-account-relay", weight: 1 },
  ];

  assert.deepEqual(
    orderAggregateMembersByCandidates(stored, [
      "openai-account-relay",
      "krill",
      "shuai-api",
    ]),
    [stored[2], stored[0], stored[1]],
  );
});

test("an empty candidate list preserves stored aggregate order", () => {
  const stored = [
    { profileId: "krill", weight: 2 },
    { profileId: "openai-account-relay", weight: 1 },
  ];

  assert.deepEqual(orderAggregateMembersByCandidates(stored, []), stored);
});

test("candidate ordering never selects an unselected aggregate member", () => {
  const selected = [{ profileId: "krill", weight: 2 }];

  assert.deepEqual(
    orderAggregateMembersByCandidates(selected, ["openai-account-relay", "krill", "shuai-api"]),
    selected,
  );
});

test("aggregate profiles preserve and expose Codex context overrides", async () => {
  const source = await readFile(new URL("./App.tsx", import.meta.url), "utf8");

  assert.match(source, /contextWindow: profile\.contextWindow \|\| ""/);
  assert.match(source, /autoCompactLimit: profile\.autoCompactLimit \|\| ""/);
  assert.match(source, /显式设置会写入 model_context_window/);
  assert.match(source, /显式设置会写入 model_auto_compact_token_limit/);
});
