import { describe, expect, it } from "vitest";
import catppuccinIconUrls from "virtual:rcode-catppuccin-icons";
import { fileExtensions, fileNames } from "@/modules/explorer/lib/fileIcons";
import { folderNames } from "@/modules/explorer/lib/folderIcons";
import { fileIconUrl, folderIconUrl } from "./iconResolver";

// These assertions lock the resolution chain (by-name, extension, compound
// extension, and default fallback) rather than the icon SVGs themselves, so
// they stay valid when the underlying icon set changes.
const DEFAULT = fileIconUrl("file-with-no-known-extension");

describe("fileIconUrl resolution chain", () => {
  it("returns a local svg resource url", () => {
    expect(fileIconUrl("a.ts")).toMatch(/\.svg$/);
  });

  it("is deterministic for the same extension", () => {
    expect(fileIconUrl("a.ts")).toBe(fileIconUrl("b.ts"));
  });

  it("resolves a known extension to something other than the default", () => {
    expect(fileIconUrl("a.ts")).not.toBe(DEFAULT);
  });

  it("distinguishes a compound extension from its base", () => {
    // .test.tsx should resolve differently from a plain .tsx.
    expect(fileIconUrl("component.test.tsx")).not.toBe(
      fileIconUrl("component.tsx"),
    );
  });

  it("resolves a by-name match (no extension) to a non-default icon", () => {
    expect(fileIconUrl("Dockerfile")).not.toBe(DEFAULT);
  });

  it("falls back to the default for an unknown extension", () => {
    expect(fileIconUrl("mystery.qzxwv")).toBe(DEFAULT);
  });
});

describe("folderIconUrl", () => {
  it("returns a local svg resource url", () => {
    expect(folderIconUrl("src", false)).toMatch(/\.svg$/);
  });

  it("uses distinct open and closed icons for a mapped folder", () => {
    const closed = folderIconUrl("src", false);
    const expanded = folderIconUrl("src", true);
    expect(expanded).toMatch(/\.svg$/);
    expect(expanded).not.toBe(closed);
  });

  it("differs from the confirmed unknown-folder fallback when mapped", () => {
    const fallback = folderIconUrl("qzxwv-dir", false);
    const fallbackExpanded = folderIconUrl("qzxwv-dir", true);
    expect(fallback).toMatch(/\.svg$/);
    expect(fallbackExpanded).toMatch(/\.svg$/);
    expect(fallbackExpanded).not.toBe(fallback);
    expect(folderIconUrl("src", false)).not.toBe(fallback);
  });
});

describe("complete icon mappings", () => {
  it("preserves every available filename and extension association", () => {
    for (const name of Object.keys(fileNames)) {
      const icon = (fileNames as Record<string, string>)[name.toLowerCase()];
      expect(fileIconUrl(name), name).toBe(fileIconUrl(name.toLowerCase()));
      if (!icon) continue;
      const expected = catppuccinIconUrls[icon.replace(/_/g, "-")];
      if (expected) expect(fileIconUrl(name), name).toBe(expected);
    }
    for (const extension of Object.keys(fileExtensions)) {
      const icon = (fileExtensions as Record<string, string>)[
        extension.toLowerCase()
      ];
      expect(fileIconUrl(`rcode-test.${extension}`), extension).toBe(
        fileIconUrl(`rcode-test.${extension.toLowerCase()}`),
      );
      if (!icon) continue;
      const expected = catppuccinIconUrls[icon.replace(/_/g, "-")];
      if (expected)
        expect(fileIconUrl(`rcode-test.${extension}`), extension).toBe(
          expected,
        );
    }
  });

  it("preserves every available folder association in both states", () => {
    for (const name of Object.keys(folderNames)) {
      const icon = (folderNames as Record<string, string>)[name.toLowerCase()];
      const slug = (icon ?? "folder").replace(/_/g, "-");
      const closed = catppuccinIconUrls[slug];
      const opened = catppuccinIconUrls[`${slug}-open`];
      if (closed) expect(folderIconUrl(name, false), name).toBe(closed);
      if (opened) expect(folderIconUrl(name, true), name).toBe(opened);
    }
  });

  it("resolves the Maven alias to the complete Apache image", () => {
    expect(catppuccinIconUrls.maven).toBe(catppuccinIconUrls.apache);
    expect(catppuccinIconUrls.maven).toBeTruthy();
    expect(fileIconUrl("pom.xml")).toBe(catppuccinIconUrls.apache);
  });
});
