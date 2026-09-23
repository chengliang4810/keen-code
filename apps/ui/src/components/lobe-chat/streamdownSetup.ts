/**
 * Streamdown 渲染栈的共享装配:插件定制、remark 默认值合并与链接协议白名单。
 *
 * 插件用法移植自 ZCode `packages/ui/src/components/ai-elements/message.tsx`
 * (Apache-2.0)的已验证组合:
 * - `singleDollarTextMath` 打开行内 `$...$` 公式(配合 lib/dollarMathGuard 防误伤);
 * - `~text~` 单波浪线删除线在 remark-gfm 与 @streamdown/cjk 两处同时关闭
 *   (两处默认都开 singleTilde 且后者覆盖前者,都关闭才符合 GFM 规范);
 * - 显式传入 remarkPlugins 会整体覆盖 streamdown 默认值,所以必须手动带上
 *   默认 gfm/codeMeta,否则表格退化为普通段落。
 */

import { cjk, type CjkPlugin } from "@streamdown/cjk";
import { createMathPlugin } from "@streamdown/math";
import remarkCjkFriendlyGfmStrikethrough from "remark-cjk-friendly-gfm-strikethrough";
import { defaultRemarkPlugins, type PluginConfig } from "streamdown";
import type { Pluggable, PluggableList } from "unified";

function disableSingleTilde(plugin: Pluggable): Pluggable {
  if (!Array.isArray(plugin)) {
    return typeof plugin === "function"
      ? [plugin, { singleTilde: false }]
      : plugin;
  }
  const [attacher, existingOptions] = plugin;
  return [
    attacher,
    {
      ...(typeof existingOptions === "object" && existingOptions !== null
        ? existingOptions
        : {}),
      singleTilde: false,
    },
  ];
}

/**
 * remark-cjk-friendly-gfm-strikethrough 必须与 @streamdown/cjk 内部解析到同一
 * 实例(pnpm dedupe),这里的 identity 比较才能定位到删除线插件并注入选项。
 */
function patchCjkSingleTilde(plugin: CjkPlugin): CjkPlugin {
  const patchedAfter = plugin.remarkPluginsAfter.map((entry) => {
    const attacher = Array.isArray(entry) ? entry[0] : entry;
    return attacher === remarkCjkFriendlyGfmStrikethrough
      ? disableSingleTilde(entry)
      : entry;
  });
  return {
    ...plugin,
    remarkPlugins: [...plugin.remarkPluginsBefore, ...patchedAfter],
    remarkPluginsAfter: patchedAfter,
  };
}

/** 聊天与资源面板共用的 KaTeX 配置(行内单 `$` 开启)。 */
const chatMathPlugin = createMathPlugin({ singleDollarTextMath: true });

/**
 * 不注册 @streamdown/code / @streamdown/mermaid:代码高亮由自家
 * shikiChatHighlighter(单份 shiki)完成,Mermaid 由 MermaidBlock
 * (settled-only + 预览对话框)完成,块级围栏在 `code` 组件覆写层即被截流,
 * 注册插件只会多带一份 shiki@3 副本。
 */
export const streamdownPlugins: PluginConfig = {
  cjk: patchCjkSingleTilde(cjk),
  math: chatMathPlugin,
};

/** streamdown 内置表格/代码/图片控件全部关闭:界面完全由 lobe-chat.css 控制。 */
export const streamdownControlsDisabled = false;

/** 链接行为由 MarkdownChat 的 `a` 覆写接管,不需要内置安全弹窗。 */
export const streamdownLinkSafety = { enabled: false } as const;

/**
 * 合并 streamdown 默认 remark 插件与调用方插件。
 * 显式 remarkPlugins 会整体替换默认值,因此必须把 gfm(带 singleTilde 修正)
 * 与 codeMeta 一并传入。
 */
export function buildStreamdownRemarkPlugins(
  extra: PluggableList,
): PluggableList {
  const defaults: PluggableList = Object.entries(defaultRemarkPlugins).map(
    ([name, plugin]) => (name === "gfm" ? disableSingleTilde(plugin) : plugin),
  );
  return [...defaults, ...extra];
}

const SAFE_URL_PROTOCOLS = new Set(["http", "https", "mailto", "tel"]);

/**
 * 链接/图片协议白名单。
 *
 * streamdown 的默认 urlTransform 是全透传(含 `javascript:`),而旧
 * react-markdown 默认按协议白名单过滤;为不放松安全边界,这里显式还原白名单,
 * 并额外放行单字母 Windows 盘符(`D:\x`、`C:/y`)供 FilePathCard / 图片解析。
 * `file://` 维持旧行为不透传(WebView 会拦截本地协议,透传只会产生加载失败)。
 */
export function chatUrlTransform(url: string): string {
  const colon = url.indexOf(":");
  if (colon < 0) return url;
  const slash = url.indexOf("/");
  const query = url.indexOf("?");
  const hash = url.indexOf("#");
  const isRelative =
    slash !== -1 &&
    slash < colon &&
    (query === -1 || slash < query) &&
    (hash === -1 || slash < hash);
  if (isRelative) return url;
  const protocol = url.slice(0, colon).toLowerCase();
  if (SAFE_URL_PROTOCOLS.has(protocol)) return url;
  if (/^[a-z]$/.test(protocol)) return url;
  return "";
}
