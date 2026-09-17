import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

test("ordinary provider switches never restart or relaunch an active Codex task", () => {
  const start = source.indexOf("const switchRelayProfile = async");
  const end = source.indexOf("const snapshotActiveRelayFilesBeforeSwitch", start);
  assert.ok(start >= 0 && end > start, "switchRelayProfile implementation must be present");

  const body = source.slice(start, end);
  assert.match(body, /call<RelaySwitchResult>\("switch_relay_profile"/);
  assert.doesNotMatch(body, /restart_codex_plus|launch_codex_plus|actions\.restart|restart\(/);
});
