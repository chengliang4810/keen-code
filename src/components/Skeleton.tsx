/**
 * 列表内容加载骨架 —— 形状贴合 .ext-item 卡片行（名称 + 描述两行）。
 * 用于替换"正在加载…"文字行：加载期间保持版面结构稳定。
 */

export function SkeletonList({
  rows = 3,
  label,
}: {
  rows?: number;
  /** 无障碍朗读用的加载文案（复用既有 i18n 键）。 */
  label: string;
}) {
  return (
    <ul className="ext-skeleton" role="status" aria-label={label}>
      {Array.from({ length: rows }, (_, index) => (
        <li key={index} className="ext-skeleton__item">
          <span className="ext-skeleton__bar ext-skeleton__bar--name" />
          <span className="ext-skeleton__bar ext-skeleton__bar--w90" />
          <span className="ext-skeleton__bar ext-skeleton__bar--w55" />
        </li>
      ))}
    </ul>
  );
}
