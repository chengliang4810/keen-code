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
    expect(html).toContain("h-5 w-9.5")
    expect(html).toContain("data-checked:bg-primary")
  })

  it("allows an explicit compact exception", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" size="sm" />,
    )

    expect(html).toContain('data-unchecked=""')
    expect(html).toContain("h-4 w-7.5")
  })

  it("keeps a semantic accessible state", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" defaultChecked />,
    )

    expect(html).toContain('role="switch"')
    expect(html).toContain('aria-checked="true"')
  })
})
