import {
  getInjectedHostTransportAdapter,
  resolveHostMode,
} from "@/components/host/hostMode";
import { isTauri } from "./tauri";

/**
 * 返回当前页面是否拥有可用的 ACP Host。
 *
 * 浏览器预览页仍然可以渲染完整 React 树，但没有 Web Host transport 时
 * 不能启动 Session；Web 模式必须同时具备注入的认证 transport，避免把
 * `isTauri() === false` 当成“远端能力可用”。
 */
export function canUseAcpHost(native = isTauri()): boolean {
  if (native) return true;
  if (resolveHostMode() !== "web") return false;
  return typeof getInjectedHostTransportAdapter()?.dispatch === "function";
}

/** 项目登记、重命名、移动、删除和排序只属于 Desktop Host。 */
export function canWriteProjects(native = isTauri()): boolean {
  return native && resolveHostMode() === "desktop";
}

/** 当前页面是否是需要完整工作台投影的本机 Web Host。 */
export function isWebHost(): boolean {
  return resolveHostMode() === "web";
}
