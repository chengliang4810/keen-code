import assert from "node:assert/strict";
import test from "node:test";
import { inspectSource, scanDesignSystem } from "./design-system-gate.mjs";

function rules(file, source) {
  return inspectSource(file, source).map((item) => item.rule);
}

test("业务 TSX 拒绝可见原生控件", () => {
  assert.deepEqual(rules("packages/ui/src/features/example.tsx", "export const View = () => <button>Run</button>;"), ["DSG001"]);
  assert.deepEqual(rules("features/example.tsx", "export const View = () => <button>Run</button>;"), ["DSG001"]);
});

test("JSX 包装组件与原生控件按大小写区分", () => {
  assert.deepEqual(rules("features/example.tsx", "export const View = () => <Button><Input /><Select /></Button>;"), []);
  assert.deepEqual(rules("features/example.tsx", "export const View = () => <Button><input /></Button>;"), ["DSG001"]);
});

test("隐藏文件输入不触发原生控件规则", () => {
  assert.deepEqual(rules("features/file.tsx", "export const View = () => <input type=\"hidden\" />;"), []);
});

test("来源 UI 的 DOM 和动态布局仍需精确来源特征才能放行", () => {
  assert.deepEqual(
    rules("packages/ui/src/components/ui/primitive.tsx", "export const View = () => <button style={{ width: size }}>Run</button>;"),
    ["DSG001", "DSG003"],
  );
});

test("业务 TSX 拒绝字面主题色和内建字号", () => {
  const found = rules(
    "features/example.tsx",
    "export const View = () => <div className=\"text-sm text-[#fff]\" />;",
  );
  assert.deepEqual(found.sort(), ["DSG002", "DSG006"]);
});

test("代码和终端内容允许独立内容字号", () => {
  assert.deepEqual(
    rules("components/code-block.tsx", "export const Code = () => <pre className=\"text-sm\" />;"),
    [],
  );
});

test("token stylesheet 的新增颜色和字号也必须通过来源特征检查", () => {
  assert.deepEqual(rules("packages/ui/src/styles.css", ".surface { color: #fff; font-size: 13px; }"), ["DSG004", "DSG006"]);
  assert.deepEqual(
    rules("features/example.css", ".surface { color: #fff; font-size: 13px; }"),
    ["DSG004", "DSG006"],
  );
});

test("固定 ZCode UI 源码通过精确基线特征放行", async () => {
  assert.deepEqual(await scanDesignSystem(["packages/ui/src"]), []);
});
