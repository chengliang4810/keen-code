import { Skeleton } from "@appica/ui-react/skeleton";

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
          <Skeleton className="ext-skeleton__bar ext-skeleton__bar--name" />
          <Skeleton className="ext-skeleton__bar ext-skeleton__bar--w90" />
          <Skeleton className="ext-skeleton__bar ext-skeleton__bar--w55" />
        </li>
      ))}
    </ul>
  );
}
