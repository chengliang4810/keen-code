/**
 * 右侧内置浏览器的地址解析。
 *
 * 地址栏输入只在这里归一化：http/https 交给原生子 WebView 加载，
 * 本地路径交给资源面板既有的文件预览链路，其余协议一律拒绝。
 */

import { isAbsoluteFsPath, pathBasename } from "@/lib/filePath";

/**
 * 新建网页标签的初始地址。
 *
 * 原生子 WebView 必须用空白页创建：Tauri 按创建时的地址推导来源，并把 `asset://`
 * 本地文件协议的 `Access-Control-Allow-Origin` 设成同一来源；直接用远程地址创建会让
 * 远程页面获得读取本机文件的能力。空白页来源为 `null`，因此这里保持 `about:blank`。
 */
export const BLANK_BROWSER_URL = "about:blank";

/** 地址栏输入的解析结果。 */
export type BrowserAddress =
  | { kind: "web"; url: string }
  | { kind: "file"; path: string };

/** 从 `file://` 地址取回本地路径；非 file 地址返回 null。 */
function fileUrlToPath(input: string): string | null {
  try {
    const url = new URL(input);
    if (url.protocol !== "file:") return null;
    const decoded = decodeURIComponent(url.pathname);
    // Windows 的 `file:///C:/x` 需要去掉盘符前的斜杠。
    return /^\/[A-Za-z]:/.test(decoded) ? decoded.slice(1) : decoded;
  } catch {
    return null;
  }
}

/** 本机回环地址默认 http，其他主机默认 https。 */
function defaultSchemeFor(host: string): string {
  return /^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/i.test(host)
    ? "http"
    : "https";
}

/**
 * 解析地址栏输入。
 *
 * 无法识别或属于脚本等危险协议时返回 null，由调用方给出错误提示。
 */
export function parseBrowserAddress(raw: string): BrowserAddress | null {
  const input = raw.trim();
  if (!input) return null;

  // 本地绝对路径优先于协议识别：`C:\x` 会被 scheme 正则误判。
  if (isAbsoluteFsPath(input)) return { kind: "file", path: input };

  const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(input)?.[1]?.toLowerCase();
  if (scheme && !isPortSuffix(input)) {
    if (scheme === "http" || scheme === "https") {
      return { kind: "web", url: input };
    }
    if (scheme === "file") {
      const path = fileUrlToPath(input);
      return path ? { kind: "file", path } : null;
    }
    return null;
  }

  // 无协议：按主机名补全。
  const host = input.split(/[/?#]/)[0] ?? input;
  return { kind: "web", url: `${defaultSchemeFor(host)}://${input}` };
}

/**
 * 判断 `host:port` 形式被误读为协议名的情况。
 *
 * 地址栏里的 `localhost:3000` 会被 scheme 正则识别出 `localhost`，
 * 只有冒号后是纯数字端口时才按主机名处理。
 */
function isPortSuffix(input: string): boolean {
  const colon = input.indexOf(":");
  return colon > 0 && /^\d+(\/|$)/.test(input.slice(colon + 1));
}

/** 从网页地址派生标签显示名。 */
export function browserTabLabel(url: string): string {
  try {
    const parsed = new URL(url);
    if (parsed.protocol === "file:") {
      const path = fileUrlToPath(url);
      return path ? pathBasename(path) : url;
    }
    return parsed.host || url;
  } catch {
    return url;
  }
}
