import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"

import { Switch } from "./switch"

describe("Switch", () => {
  it("renders the Radix switch with the token-backed default size", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" defaultChecked />,
    )

    expect(html).toContain('data-slot="switch"')
    expect(html).toContain('data-slot="switch-thumb"')
    expect(html).toContain('data-size="default"')
    expect(html).toContain('data-state="checked"')
    expect(html).toContain(
      "data-[state=checked]:translate-x-[calc(100%+2px)]",
    )
  })

  it("supports the compact size", () => {
    const html = renderToStaticMarkup(
      <Switch aria-label="启用通知" size="sm" />,
    )

    expect(html).toContain('data-size="sm"')
    expect(html).toContain('data-state="unchecked"')
  })
})
