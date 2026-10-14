import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyKeyError, INVALID_KEY_CHARS_MESSAGE } from "./errors.ts";
import type { ErrorCode } from "./types.ts";

const INLINE: ErrorCode[] = ["emptyKey", "keyRejected", "invalidKeyChars"];
const MESSAGE_ONLY: ErrorCode[] = [
  "credentialMissing",
  "portUnavailable",
  "configInvalid",
  "credential",
  "io",
  "internal",
  "installLocation",
];

test("emptyKey / keyRejected / invalidKeyChars 就地提示", () => {
  for (const code of INLINE) {
    assert.equal(classifyKeyError({ code, message: "x" }), "inline");
  }
});

test("gatewayUnavailable 走「仍然保存」交互", () => {
  assert.equal(classifyKeyError({ code: "gatewayUnavailable", message: "网关暂时不可用" }), "gatewayUnavailable");
});

test("catalogConfirmationRequired 走确认模态框", () => {
  assert.equal(
    classifyKeyError({ code: "catalogConfirmationRequired", message: "需要确认移除" }),
    "catalogConfirmation",
  );
});

test("其余错误码只展示 message", () => {
  for (const code of MESSAGE_ONLY) {
    assert.equal(classifyKeyError({ code, message: "x" }), "message");
  }
});

test("invalidKeyChars 文案与 Rust 侧一致", () => {
  assert.equal(INVALID_KEY_CHARS_MESSAGE, "Key 只能包含英文字母、数字和符号");
});
