import { uiState } from "@/lib/uiState";
export function terminalDiagnosticsEnabled(): boolean {
  if (typeof window === "undefined") return false;
  if (import.meta.env.DEV) return true;
  try {
    return uiState.getItem("rcode:terminal-diagnostics") === "1";
  } catch {
    return false;
  }
}
