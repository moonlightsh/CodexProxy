import assert from "node:assert/strict";
import { test } from "node:test";
import { downloadView, updateDownloadProgress, type ComponentDownloadState } from "./componentdownload.ts";

const downloading: ComponentDownloadState = {
  kind: "downloading", id: "one", receivedBytes: 1024, totalBytes: 4096, started: true, cancelling: false,
};

test("下载进度显示字节与比例，未知总长度不伪造百分比", () => {
  const known = downloadView(downloading);
  assert.equal(known.percent, 25);
  assert.match(known.text, /1\.0 KiB.*4\.0 KiB/);
  assert.equal(known.canCancel, true);
  assert.equal(downloadView({ ...downloading, totalBytes: null }).percent, null);
});

test("取消后显示取消状态，准备和取消阶段禁用取消按钮", () => {
  assert.equal(downloadView({ ...downloading, cancelling: true }).canCancel, false);
  assert.match(downloadView({ ...downloading, cancelling: true }).text, /取消/);
  assert.equal(downloadView({ ...downloading, started: false }).canCancel, false);
  assert.match(downloadView({ kind: "cancelled" }).text, /已取消/);
});

test("完成后展示文件路径，失败后展示原因并允许重试", () => {
  const done = downloadView({ kind: "completed", version: "0.166.0", path: "/Downloads/CodexHelper/codex.zip" });
  assert.equal(done.path, "/Downloads/CodexHelper/codex.zip");
  assert.equal(done.canCancel, false);
  assert.match(done.text, /0\.166\.0/);
  assert.match(downloadView({ kind: "failed", message: "代理连接失败" }).text, /代理连接失败/);
});

test("忽略旧任务和完成后的迟到进度", () => {
  const progress = { receivedBytes: 2048, totalBytes: 4096 };
  assert.equal(updateDownloadProgress(downloading, "old", progress), downloading);
  const done: ComponentDownloadState = { kind: "completed", version: "0.166.0", path: "/a.zip" };
  assert.equal(updateDownloadProgress(done, "one", progress), done);
  const current = updateDownloadProgress(downloading, "one", progress);
  assert.equal(current?.kind === "downloading" && current.receivedBytes, 2048);
});

test("任务正在取消时迟到进度不会恢复取消按钮", () => {
  const current = updateDownloadProgress({ ...downloading, cancelling: true }, "one", { receivedBytes: 2048, totalBytes: null });
  assert.equal(current?.kind === "downloading" && current.cancelling, true);
});
