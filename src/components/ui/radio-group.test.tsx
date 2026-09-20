import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"

import { RadioGroup } from "@appica/ui-react/radio-group"
import { Radio } from "@appica/ui-react/radio"

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
})
