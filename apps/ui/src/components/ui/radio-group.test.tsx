import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"

import { Radio, RadioGroup } from "@/components/ui/radio-group"
import { Checkbox, CheckboxGroup } from "@/components/ui/checkbox-group"

describe("Radio", () => {
  it("renders Appica checked state and indicator", () => {
    const html = renderToStaticMarkup(
      <RadioGroup defaultValue="rose">
        <Radio value="rose" aria-label="玫瑰" />
      </RadioGroup>,
    )

    expect(html).toContain('data-slot="radio"')
    expect(html).toContain('data-checked=""')
    expect(html).toContain('data-slot="radio-indicator"')
  })

  it("renders an unchecked radio outside of any selection", () => {
    const html = renderToStaticMarkup(
      <RadioGroup value="">
        <Radio value="rose" aria-label="玫瑰" />
      </RadioGroup>,
    )

    expect(html).toContain('data-slot="radio"')
    expect(html).not.toContain('data-checked=""')
  })
})

describe("Checkbox", () => {
  it("renders Appica checked state and indicator", () => {
    const html = renderToStaticMarkup(
      <CheckboxGroup defaultValue={["lint"]}>
        <Checkbox value="lint" aria-label="Lint" />
      </CheckboxGroup>,
    )

    expect(html).toContain('data-slot="checkbox"')
    expect(html).toContain('data-checked=""')
    expect(html).toContain('data-slot="checkbox-indicator"')
  })
})
