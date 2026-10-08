type DirectoryAccess = {
  canonicalize: (path: string) => Promise<string>;
  stat: (path: string) => Promise<{ kind: string }>;
};

/** 手填名称优先；空名称跟随当前目录，不将自动推断的名称当成用户输入。 */
export function projectNameForDirectory(
  name: string,
  root: string,
  fallback: string,
): string {
  const segments = root.split(/[\\/]/).filter(Boolean);
  return name.trim() || segments[segments.length - 1] || fallback;
}

/** 选择和拖拽共用校验；先解析链接，再确认目录，整个过程不扩大工作区授权。 */
export async function resolveProjectDirectory(
  paths: string[],
  access: DirectoryAccess,
): Promise<string> {
  if (paths.length !== 1) throw new Error("Add one folder at a time.");
  const path = paths[0];
  if (!/^([A-Za-z]:[\\/]|\/|\\\\)/.test(path))
    throw new Error("Use an absolute project directory.");
  const canonical = await access.canonicalize(path);
  if ((await access.stat(canonical)).kind !== "dir")
    throw new Error("Select a folder instead of a file.");
  return canonical;
}

export type ProjectDirectoryDrop =
  | { type: "enter" | "over"; position: { x: number; y: number } }
  | { type: "drop"; position: { x: number; y: number }; paths: string[] }
  | { type: "leave" };

/** Tauri 的拖拽坐标为物理像素，命中判断必须转换成表单的 CSS 像素。 */
export function createProjectDirectoryDropTarget({
  contains,
  onHover,
  onDrop,
  pixelRatio,
}: {
  contains: (x: number, y: number) => boolean;
  onHover: (active: boolean) => void;
  onDrop: (paths: string[]) => void;
  pixelRatio: () => number;
}) {
  return (event: ProjectDirectoryDrop) => {
    if (event.type === "leave") {
      onHover(false);
      return;
    }
    const ratio = pixelRatio() || 1;
    const inside = contains(event.position.x / ratio, event.position.y / ratio);
    if (event.type === "drop") {
      onHover(false);
      if (inside) onDrop(event.paths);
    } else onHover(inside);
  };
}
