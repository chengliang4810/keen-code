import { describe, expect, it, vi } from "vitest"
import { renderToStaticMarkup } from "react-dom/server"

import { MultiSelect } from "./multi-select"

const options = [
  { value: "read", label: "读取" },
  { value: "write", label: "写入" },
  { value: "shell", label: "终端", disabled: true },
] as const

describe("MultiSelect", () => {
  it("renders a labelled shadcn trigger and drops unknown or duplicate values", () => {
    const html = renderToStaticMarkup(
      <MultiSelect
        options={options}
        value={["unknown", "write", "write", "read"]}
        onValueChange={vi.fn()}
        placeholder="选择工具"
        ariaLabel="工具"
        ariaDescribedBy="tools-help"
      />,
    )

    expect(html).toContain('data-slot="multi-select-trigger"')
    expect(html).toContain('aria-label="工具"')
    expect(html).toContain('aria-describedby="tools-help"')
    expect(html).toContain('aria-expanded="false"')
    expect(html).toContain("读取, 写入")
    expect(html).not.toContain("unknown")
  })

  it("lets callers own the selected summary", () => {
    const html = renderToStaticMarkup(
      <MultiSelect
        options={options}
        value={["read"]}
        onValueChange={vi.fn()}
        placeholder="选择工具"
        ariaLabel="工具"
        renderValue={(selected) => `已选 ${selected.length} 项`}
      />,
    )

    expect(html).toContain("已选 1 项")
  })
})
