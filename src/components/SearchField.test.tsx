import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SearchField } from "./SearchField";

describe("SearchField", () => {
  it("保留业务容器类名并把输入属性透传给 shadcn Input", () => {
    const markup = renderToStaticMarkup(
      <SearchField
        containerClassName="resource-search"
        iconSize={17}
        value="needle"
        readOnly
        aria-label="搜索资源"
      />,
    );

    expect(markup).toContain('class="resource-search"');
    expect(markup).toContain('width="17"');
    expect(markup).toContain('data-slot="input"');
    expect(markup).toContain('aria-label="搜索资源"');
    expect(markup).toContain('value="needle"');
  });
});
