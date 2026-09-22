import test from "node:test";
import assert from "node:assert/strict";
import { inspectSource } from "./design-system-gate.mjs";

test("design-system gate rejects visible native controls", () => {
  const violations = inspectSource("apps/ui/src/components/Example.tsx", "export function Example() { return <button>保存</button>; }");
  assert.ok(violations.some((item) => item.rule === "DSG001"));
});

test("design-system gate permits hidden file input and custom properties", () => {
  const violations = inspectSource(
    "apps/ui/src/components/Example.tsx",
    '<input type="file" hidden />\n<div style={{ "--height": "20px" } as CSSProperties} />',
  );
  assert.deepEqual(violations, []);
});

test("design-system gate reports raw colors and CSS literals", () => {
  const tsViolations = inspectSource("apps/ui/src/components/Example.tsx", 'const color = "#fff";');
  const cssViolations = inspectSource("apps/ui/src/components/example.css", ".x { color: #fff; }");
  assert.ok(tsViolations.some((item) => item.rule === "DSG002"));
  assert.ok(cssViolations.some((item) => item.rule === "DSG004"));
});

test("design-system gate honors explicit token and host allowlists", () => {
  assert.deepEqual(inspectSource("apps/ui/src/styles/tokens.css", ":root { --brand: #fff; }"), []);
  assert.deepEqual(inspectSource("apps/ui/src/components/TerminalPanel.tsx", 'const theme = { background: "#000" };'), []);
});

test("design-system gate requires md for Appica size props", () => {
  const violations = inspectSource(
    "apps/ui/src/components/Example.tsx",
    `
      import { Badge } from "@appica/ui-react/badge";
      import { Input } from "@appica/ui-react/input";
      <Badge size="xs">状态</Badge>;
      <Input inputSize="sm" />;
    `,
  );
  assert.equal(violations.filter((item) => item.rule === "DSG005").length, 2);
});

test("design-system gate requires an explicit md size for Appica controls", () => {
  const violations = inspectSource(
    "apps/ui/src/components/Example.tsx",
    `
      import { NumberField } from "@appica/ui-react/number-field";
      import { Textarea } from "@appica/ui-react/textarea";
      <NumberField />;
      <Textarea />;
    `,
  );
  assert.equal(violations.filter((item) => item.rule === "DSG005").length, 2);
});

test("design-system gate accepts md for every Appica size", () => {
  const violations = inspectSource(
    "apps/ui/src/components/Example.tsx",
    `
      import { Avatar } from "@appica/ui-react/avatar";
      import { Badge } from "@appica/ui-react/badge";
      <Avatar size="md" />;
      <Badge size="md">状态</Badge>;
    `,
  );
  assert.deepEqual(violations.filter((item) => item.rule === "DSG005"), []);
});

test("design-system gate requires md for Appica CopyButton", () => {
  const violations = inspectSource(
    "apps/ui/src/components/CopyAction.tsx",
    `import { CopyButton } from "@appica/ui-react/copy-button";
      export function CopyAction() {
        return <CopyButton value="text" />;
      }`,
  );
  assert.equal(violations.filter((item) => item.rule === "DSG005").length, 1);
});

test("design-system gate rejects non-md Avatar sizes including pixel expressions", () => {
  const violations = inspectSource(
    "apps/ui/src/components/Example.tsx",
    `
      import { Avatar } from "@appica/ui-react/avatar";
      <Avatar size="sm" />;
      <Avatar size={24} />;
    `,
  );
  assert.equal(violations.filter((item) => item.rule === "DSG005").length, 2);
});

test("design-system gate does not classify local Button icon semantics as Appica sizes", () => {
  const violations = inspectSource(
    "apps/ui/src/components/Example.tsx",
    `
      import { Button } from "@/components/ui/button";
      <Button size="icon-sm" aria-label="关闭" />;
    `,
  );
  assert.deepEqual(violations.filter((item) => item.rule === "DSG005"), []);
});
