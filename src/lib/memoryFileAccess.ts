/** 记忆编辑器的读取代次与在途写入队列；不保存第二份记忆正文。 */
export interface MemoryFileAccessState {
  /** 新读、保存和重置都会使旧读取回执失效。 */
  revision: number;
  /** 串行执行保存与重置，防止离开页面后旧写覆盖新写。 */
  writeTail: Promise<void>;
}

/** 创建单个应用实例的记忆文件访问栅栏。 */
export function createMemoryFileAccessState(): MemoryFileAccessState {
  return { revision: 0, writeTail: Promise.resolve() };
}

/** 只应用最新读取；等在途保存或重置完成后再读取，不增加后台轮询。 */
export async function refreshMemoryFile(
  state: MemoryFileAccessState,
  read: () => Promise<string>,
  apply: (value: string) => void,
): Promise<void> {
  const revision = ++state.revision;
  await state.writeTail;
  if (state.revision !== revision) return;
  const value = await read();
  if (state.revision === revision) apply(value);
}

/** 写入前后均作废旧读；失败仍向调用方抛出，并且不阻塞后续操作。 */
export async function writeMemoryFile(
  state: MemoryFileAccessState,
  write: () => Promise<string>,
  apply: (value: string) => void,
): Promise<void> {
  ++state.revision;
  const operation = state.writeTail.then(async () => {
    try {
      const value = await write();
      apply(value);
    } finally {
      ++state.revision;
    }
  });
  state.writeTail = operation.catch(() => {});
  await operation;
}
