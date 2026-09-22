/** 仅由本机 Git 命令包装层构造，保留 stdout/stderr 中可操作的失败原因。 */
export class LocalGitError extends Error {
  constructor(cause: unknown) {
    const text = cause instanceof Error ? cause.message : String(cause ?? "");
    super(text.trim().slice(0, 4096));
  }
}
