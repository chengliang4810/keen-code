import { describe, expect, it } from "vitest";
import { cn } from "@/lib/utils";

describe("interface font class merging", () => {
  it("replaces primitive font sizes while retaining text colors", () => {
    expect(cn("text-sm text-foreground", "text-ui-13 text-foreground/75")).toBe(
      "text-ui-13 text-foreground/75",
    );
    expect(cn("text-ui-11.5 text-muted-foreground", "text-ui-13")).toBe(
      "text-muted-foreground text-ui-13",
    );
    expect(cn("text-ui-13", "text-sm text-destructive")).toBe(
      "text-sm text-destructive",
    );
  });

  it("resolves responsive and line-height conflicts independently", () => {
    expect(
      cn("text-base md:text-sm md:text-foreground", "text-ui-13 md:text-ui-13"),
    ).toBe("md:text-foreground text-ui-13 md:text-ui-13");
    expect(cn("text-sm/6 leading-relaxed", "text-ui-12")).toBe(
      "leading-relaxed text-ui-12",
    );
  });

  it("recognizes semantic sizes without treating them as text colors", () => {
    expect(cn("text-sm text-foreground", "text-ui-base")).toBe(
      "text-foreground text-ui-base",
    );
    expect(cn("text-ui-sm text-muted-foreground", "text-ui-caption")).toBe(
      "text-muted-foreground text-ui-caption",
    );
    expect(cn("text-ui-xs text-ui-lg text-ui-xl", "text-ui-base")).toBe(
      "text-ui-base",
    );
    expect(cn("text-ui-base", "text-sm text-destructive")).toBe(
      "text-sm text-destructive",
    );
  });

  it("overrides responsive input sizes while retaining explicit line heights", () => {
    expect(
      cn(
        "text-base md:text-sm md:text-foreground",
        "text-ui-base md:text-ui-base",
      ),
    ).toBe("md:text-foreground text-ui-base md:text-ui-base");
    expect(
      cn("text-sm/6 leading-relaxed text-foreground", "text-ui-base"),
    ).toBe("leading-relaxed text-foreground text-ui-base");
  });
});
