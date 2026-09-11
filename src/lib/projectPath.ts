/**
 * Project folder path health (D05).
 * 列表返回未知状态；展开时由后端校验，前端不推断目录存在性。
 */

/** True when Host reported the project path is missing / not a directory. */
export function isProjectPathMissing(
  pathOk: boolean | undefined | null,
): boolean {
  return pathOk === false;
}
