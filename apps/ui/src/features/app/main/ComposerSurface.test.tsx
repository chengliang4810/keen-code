import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AttachmentCard } from "@/components/AttachmentCard";
import { ComposerEditor } from "@/components/ComposerEditor";
import { readSource } from "@/test-utils/readCssSource";

const attachmentLabels = {
  open: "打开",
  reveal: "显示",
  copyPath: "复制路径",
  copyImage: "复制图片",
  addToComposer: "添加到输入框",
  remove: "移除",
};

describe("Composer surface alignment", () => {
  it("keeps the empty placeholder at the editor text baseline", () => {
    const html = renderToString(
      <ComposerEditor
        value=""
        onChange={() => {}}
        ariaLabel="输入"
        placeholder="随心输入"
      />,
    );
    const css = [
      readSource(new URL("../../../styles/app-features.css", import.meta.url)),
      readSource(new URL("../../../styles/app-conversation.css", import.meta.url)),
    ].join("\n");

    expect(html).toContain("composer-editor__placeholder");
    expect(html).toContain("随心输入");
    expect(css).toMatch(
      /\.composer-editor__placeholder\s*\{[^}]*left:\s*0;[^}]*top:\s*0;[^}]*line-height:\s*calc\(20px \+ var\(--ui-font-delta\)\);/s,
    );
    expect(css).toMatch(
      /\.composer__input\s*\{[^}]*line-height:\s*calc\(20px \+ var\(--ui-font-delta\)\);/s,
    );
  });

  it("shows a normal file type subtitle without changing the 48px chip", () => {
    const html = renderToString(
      <AttachmentCard
        variant="chip"
        attachment={{
          path: "C:\\work\\notes.md",
          name: "notes.md",
          isDir: false,
          contentType: "text/markdown",
        }}
        labels={attachmentLabels}
      />,
    );
    const css = readSource(new URL("../../../styles/app-conversation.css", import.meta.url));

    expect(html).toContain('class="attach-chip__type"');
    expect(html).toContain(">MD</span>");
    expect(css).toMatch(/\.attach-chip\s*\{[^}]*--attach-h:\s*48px;/s);
    expect(css).toMatch(/\.attach-chip--image\s*\{[^}]*width:\s*var\(--attach-h\);/s);
  });

  it("connects file drop handling and priority-based toolbar fitting", () => {
    const inputSource = readSource(new URL("./ComposerInputArea.tsx", import.meta.url));
    const toolbarSource = readSource(new URL("./ComposerToolbar.tsx", import.meta.url));
    const hookSource = readSource(
      new URL("../../../hooks/composer/useComposerToolbarFit.ts", import.meta.url),
    );
    const conversationCss = readSource(
      new URL("../../../styles/app-conversation.css", import.meta.url),
    );
    const resourceCss = readSource(
      new URL("../../../styles/app-resource.css", import.meta.url),
    );

    expect(inputSource).toContain("collectLocalPathsFromDataTransfer");
    expect(inputSource).toContain("collectFilesFromDataTransfer");
    expect(inputSource).toContain("composer-editor-dropzone__hint");
    expect(inputSource).toContain('className="w-(--anchor-width) !max-w-none p-0"');
    expect(inputSource).toContain("setIsDraggingFiles(false)");
    expect(toolbarSource).toContain("useComposerToolbarFit");
    expect(toolbarSource).toContain('variant="ghost"');
    expect(toolbarSource).toContain("<IconPlus size={16} />");
    expect(hookSource).toContain("ResizeObserver");
    expect(hookSource).toContain("data-composer-collapse-priority");
    expect(resourceCss).toContain("--composer-model-max-width");
    expect(conversationCss).toMatch(
      /\.composer-plus-trigger\.is-open\s*\{[^}]*transform:\s*none;/s,
    );
    expect(conversationCss).toMatch(
      /\.composer-plus--portal\s*\{[^}]*position:\s*static;/s,
    );
    expect(conversationCss).toMatch(
      /\.composer-plus\s*\{[^}]*width:\s*100%\s*!important;/s,
    );
    expect(conversationCss).toMatch(
      /\.composer-mention-panel\s*\{[^}]*width:\s*100%\s*!important;/s,
    );
    expect(conversationCss).toMatch(
      /\.prompt-history\s*\{[^}]*width:\s*100%\s*!important;/s,
    );
    expect(conversationCss).not.toMatch(
      /\.composer-plus--portal\s*\{[^}]*position:\s*fixed;/s,
    );
  });

  it("keeps Send visible for an existing draft while a turn can be stopped", () => {
    const toolbarSource = readSource(new URL("./ComposerToolbar.tsx", import.meta.url));

    expect(toolbarSource).toContain("{effectiveCanStop && !hasBody ? (");
    expect(toolbarSource).toContain('aria-label={tr("composer.stop")}');
    expect(toolbarSource).toContain("disabled={sendDisabled}");
    expect(toolbarSource).not.toContain(
      "hasConfiguredModel && hasBody && canQueue && !hasUnreadyAttachment",
    );
  });

  it("keeps the draft context surface aligned with ZCode without changing session Composer", () => {
    const css = readSource(new URL("../../../styles/app-conversation.css", import.meta.url));
    const contextSurface =
      css.match(/\.composer-stack--with-context\s*\{([^}]*)\}/s)?.[1] ?? "";
    const sessionComposer = css.match(/\.composer\s*\{([^}]*)\}/s)?.[1] ?? "";

    expect(contextSurface).toContain("border-radius: var(--radius-composer);");
    expect(contextSurface).toContain("background: var(--background-subtle);");
    expect(contextSurface).toContain("box-shadow: var(--shadow-composer-context);");
    expect(contextSurface).not.toMatch(/\bborder(?::|(?!-radius\b)-[\w-]+)\s*:/);
    expect(css).not.toMatch(/\.composer-stack--with-context \.composer\s*\{/);
    expect(css).not.toMatch(
      /\.composer-wrap--welcome \.composer-stack--with-context \.composer\s*\{[^}]*box-shadow:\s*none;/s,
    );
    expect(sessionComposer).toContain("border: 1px solid var(--border-subtle);");
  });

  it("shows the welcome greeting without a product logo", () => {
    const stageSource = readSource(new URL("../MainStage.tsx", import.meta.url));
    const css = readSource(new URL("../../../styles/app-conversation.css", import.meta.url));

    expect(stageSource).not.toContain('src="/logo.png"');
    expect(stageSource).not.toContain("composer-welcome__logo");
    expect(css).toMatch(
      /\.composer-wrap--welcome\s*\{[^}]*position:\s*relative;[^}]*flex:\s*1 1 auto;[^}]*justify-content:\s*center;/s,
    );
    expect(css).not.toContain(".composer-welcome__logo");
    expect(css).not.toContain("29dvh");
  });
});
