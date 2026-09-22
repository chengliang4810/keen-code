/** 按需将权威文件快照转换为差异预览；不访问 Git 或工作区当前文件。 */
import type { MessageFileChange } from "./session";
import { loadFileChangeSnapshot, type FileChangeReadRequest } from "./acp/fileChanges";
import { buildUnifiedDiff } from "./sessionChanges";

/** 从已确认应用的快照生成完整差异；准备阶段与读取失败不能伪装为已写入。 */
export async function loadSnapshotDiff(
  change: MessageFileChange,
  request: FileChangeReadRequest,
  signal?: AbortSignal,
): Promise<string> {
  if (change.reference) {
    if (change.reference.path !== change.path) throw new Error("文件快照路径与引用不匹配");
    if (!change.reference.applied) {
      throw new Error("仅保存了写入前后的准备快照，尚未确认文件实际修改成功。请检查工具执行结果。");
    }
    // 两侧顺序读取，避免同时保留两组分页解码缓冲；正文只在打开差异时加载。
    const before = await loadFileChangeSnapshot(change.reference, "before", request, signal);
    const after = await loadFileChangeSnapshot(change.reference, "after", request, signal);
    if (after === null) throw new Error("文件变更缺少写入后快照");
    if (before?.includes("\0") || after.includes("\0")) {
      throw new Error("文件快照包含二进制数据，不能作为文本差异展示");
    }
    return buildUnifiedDiff(change.path, before ?? "", after);
  }
  if (change.oldText?.includes("\0") || change.newText.includes("\0")) {
    throw new Error("文件快照包含二进制数据，不能作为文本差异展示");
  }
  return buildUnifiedDiff(change.path, change.oldText ?? "", change.newText);
}
