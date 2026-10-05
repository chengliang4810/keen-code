export type InterfaceMode = "office" | "coding";

export const INTERFACE_MODE_STORAGE_KEY = "zcode-interface-mode";

export function normalizeInterfaceMode(_value: unknown): InterfaceMode {
  // KeenCode 只提供编程界面；持久值、跨窗口广播和外部调用均不能开启办公模式。
  return "coding";
}
