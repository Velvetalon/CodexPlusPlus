import assert from "node:assert";
import { describe, it } from "node:test";
import type { RelayProfile } from "./App.tsx";
import {
  buildModelWindows,
  modelWindowRowsFromProfile,
  modelWindowsMapToText,
  modelWindowsTextToMap,
  serializeModelWindowRows,
  mergeModelWindowRows,
} from "./model-windows.ts";

// 类型检查：确保 RelayProfile 包含 modelWindows 和 modelVlm 字段
const _profileTypeCheck: RelayProfile = {
  id: "test",
  name: "",
  model: "",
  baseUrl: "",
  upstreamBaseUrl: "",
  apiKey: "",
  protocol: "responses",
  responsesReasoningPolicy: "passthrough",
  responsesWirePolicy: "compatible",
  nativeAgentInterop: "auto",
  relayMode: "official",
  officialMixApiKey: false,
  noAuth: false,
  hideOfficialUsageAlert: false,
  testModel: "",
  configContents: "",
  authContents: "",
  useCommonConfig: true,
  contextWindow: "",
  autoCompactLimit: "",
  modelList: "",
  modelWindows: "",
  modelVlm: "",
  vlmApiKey: "",
  vlmModel: "",
  vlmBaseUrl: "",
  userAgent: "",
  sub2apiEnabled: false,
  sub2apiMultiplier: "",
};

void _profileTypeCheck;

describe("model-windows helpers", () => {
  it("modelWindowsMapToText 按 modelList 行顺序输出窗口文本", () => {
    assert.strictEqual(
      modelWindowsMapToText("a\nb\nc", '{"a":"1M","c":"200K"}'),
      "1M\n\n200K",
    );
  });

  it("modelWindowsMapToText 对非法 JSON 返回空字符串", () => {
    assert.strictEqual(modelWindowsMapToText("a\nb", "not-json"), "");
  });

  it("modelWindowsTextToMap 按行组装 model_windows map", () => {
    assert.strictEqual(
      modelWindowsTextToMap("a\nb\nc", "1M\n\n200K"),
      '{"a":"1M","c":"200K"}',
    );
  });

  it("modelWindowsTextToMap 对没有对应窗口的模型不写入 map", () => {
    assert.strictEqual(
      modelWindowsTextToMap("a\nb", "1M"),
      '{"a":"1M"}',
    );
  });

  it("buildModelWindows 行数一致时返回 modelWindows JSON", () => {
    const result = buildModelWindows("deepseek-v4-flash\ndeepseek-v4-pro", "1M\n");
    assert.strictEqual(result.ok, true);
    if (result.ok) {
      assert.strictEqual(result.modelWindows, '{"deepseek-v4-flash":"1M"}');
    }
  });

  it("buildModelWindows 行数不一致时返回错误", () => {
    const result = buildModelWindows("a\nb", "1M");
    assert.strictEqual(result.ok, false);
    if (!result.ok) {
      assert.ok(result.error.includes("2"));
      assert.ok(result.error.includes("1"));
    }
  });

  it("modelWindowRowsFromProfile 把模型和窗口合成同一组行", () => {
    assert.deepStrictEqual(
      modelWindowRowsFromProfile("a\nb\nc", '{"a":"1M","c":"200K"}'),
      [
        { model: "a", aliases: "", window: "1M", imageHandling: "send-as-is" },
        { model: "b", aliases: "", window: "", imageHandling: "send-as-is" },
        { model: "c", aliases: "", window: "200K", imageHandling: "send-as-is" },
      ],
    );
  });

  it("modelWindowRowsFromProfile 解析 modelVlm 标记", () => {
    assert.deepStrictEqual(
      modelWindowRowsFromProfile("a\nb\nc", '{}', '{"a":"vlm","b":"strip"}'),
      [
        { model: "a", aliases: "", window: "", imageHandling: "vlm" },
        { model: "b", aliases: "", window: "", imageHandling: "strip" },
        { model: "c", aliases: "", window: "", imageHandling: "send-as-is" },
      ],
    );
  });

  it("modelWindowRowsFromProfile 解析模型别名并按模型分组", () => {
    assert.deepStrictEqual(
      modelWindowRowsFromProfile(
        "glm-5.3\nluna-flash",
        "{}",
        "{}",
        '{"luna":"glm-5.3","flash":"luna-flash"}',
      ),
      [
        { model: "glm-5.3", aliases: "luna", window: "", imageHandling: "send-as-is" },
        { model: "luna-flash", aliases: "flash", window: "", imageHandling: "send-as-is" },
      ],
    );
  });

  it("serializeModelWindowRows 从行控件生成 modelList、modelWindows 和 modelVlm", () => {
    assert.deepStrictEqual(
      serializeModelWindowRows([
        { model: "a", aliases: "luna", window: "1M", imageHandling: "vlm" },
        { model: "", aliases: "", window: "400K", imageHandling: "send-as-is" },
        { model: "b", aliases: "", window: "", imageHandling: "send-as-is" },
      ]),
      {
        modelList: "a\nb",
        modelWindows: '{"a":"1M"}',
        modelVlm: '{"a":"vlm"}',
        modelAliases: [{ alias: "luna", model: "a" }],
      },
    );
  });

  it("mergeModelWindowRows 追加上游模型时跳过已有模型并保留窗口和图片处理", () => {
    assert.deepStrictEqual(
      mergeModelWindowRows(
        [
          { model: "deepseek-v4-flash", aliases: "flash", window: "1M", imageHandling: "vlm" },
          { model: "  ", aliases: "", window: "", imageHandling: "send-as-is" },
        ],
        [
          { model: "deepseek-v4-flash", aliases: "", window: "", imageHandling: "send-as-is" },
          { model: "deepseek-v4-pro", aliases: "", window: "", imageHandling: "vlm" },
          { model: " deepseek-v4-pro ", aliases: "", window: "200K", imageHandling: "send-as-is" },
        ],
      ),
      [
        { model: "deepseek-v4-flash", aliases: "flash", window: "1M", imageHandling: "vlm" },
        { model: "deepseek-v4-pro", aliases: "", window: "", imageHandling: "vlm" },
      ],
    );
  });
});
