import assert from "node:assert/strict";
import test from "node:test";

import {
  formatCooldownDuration,
  relayCooldownReasonLabel,
  relayPriorityHighlights,
  type RelayCooldownMemberStatus,
} from "./relay-cooldown.ts";

function status(relayId: string, cooldownRemainingSeconds: number): RelayCooldownMemberStatus {
  return {
    relayId,
    consecutiveFailures: 0,
    failureThreshold: 3,
    cooldownRemainingSeconds,
    lastCooldownReason: null,
  };
}

test("formats cooldown durations without dropping the final partial second", () => {
  assert.equal(formatCooldownDuration(0), "0:00");
  assert.equal(formatCooldownDuration(59.2), "1:00");
  assert.equal(formatCooldownDuration(180), "3:00");
  assert.equal(formatCooldownDuration(3661), "1:01:01");
});

test("formats structured cooldown reasons", () => {
  assert.equal(relayCooldownReasonLabel({ type: "httpStatus", statusCode: 429 }, "transport"), "HTTP 429");
  assert.equal(relayCooldownReasonLabel({ type: "transportFailure" }, "transport"), "transport");
  assert.equal(relayCooldownReasonLabel(null, "transport"), "");
});

test("highlights the first available provider and its next fallback", () => {
  assert.deepEqual(
    relayPriorityHighlights(
      ["openai", "krill", "shuai"],
      [status("openai", 0), status("krill", 0), status("shuai", 0)],
    ),
    { currentRelayId: "openai", nextRelayId: "krill" },
  );
});

test("skips cooling providers for both current and next highlights", () => {
  assert.deepEqual(
    relayPriorityHighlights(
      ["openai", "krill", "shuai"],
      [status("openai", 120), status("krill", 0), status("shuai", 0)],
    ),
    { currentRelayId: "krill", nextRelayId: "shuai" },
  );
});

test("uses the earliest recovery when no other provider is currently available", () => {
  assert.deepEqual(
    relayPriorityHighlights(
      ["openai", "krill", "shuai"],
      [status("openai", 0), status("krill", 90), status("shuai", 30)],
    ),
    { currentRelayId: "openai", nextRelayId: "shuai" },
  );
});
