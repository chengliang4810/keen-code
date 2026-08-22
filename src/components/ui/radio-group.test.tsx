import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"

import { RadioGroup, RadioGroupItem } from "./radio-group"

describe("RadioGroupItem", () => {
  it("preserves custom card content alongside the Radix indicator", () => {
    const html = renderToStaticMarkup(
      <RadioGroup defaultValue="rose">
        <RadioGroupItem value="rose">
          <span className="skin-swatch" />
          <span>玫瑰</span>
        </RadioGroupItem>
      </RadioGroup>,
    )

    expect(html).toContain('class="skin-swatch"')
    expect(html).toContain("玫瑰")
    expect(html).toContain('data-slot="radio-group-indicator"')
  })
})
