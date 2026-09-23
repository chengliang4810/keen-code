import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"

import { Switch } from "@/components/ui/switch"

describe("Switch", () => {
  it("renders the Appica switch with its checked state", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" defaultChecked />,
    )

    expect(html).toContain('data-slot="switch"')
    expect(html).toContain('data-slot="switch-thumb"')
    expect(html).toContain('data-checked=""')
    // 轨道几何交由官方 Appica md 尺寸，不再使用 ZCode 像素锁。
    expect(html).toContain("h-5")
    expect(html).toContain("w-9.5")
    expect(html).not.toContain("h-[18px] w-8")
    expect(html).toContain("data-checked:bg-primary")
  })

  it("keeps the extended hit area", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" />,
    )

    expect(html).toContain('data-unchecked=""')
    expect(html).toContain("after:-inset-x-3 after:-inset-y-2")
  })

  it("keeps a semantic accessible state", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" defaultChecked />,
    )

    expect(html).toContain('role="switch"')
    expect(html).toContain('aria-checked="true"')
  })
})
