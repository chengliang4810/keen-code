import type { ComponentProps } from "react";
import { IconSearch } from "@/components/icons";
import { Input } from "@/components/ui/input";

/** 带统一搜索图标容器的 shadcn Input 组合属性。 */
export interface SearchFieldProps extends ComponentProps<typeof Input> {
  /** 保留各业务域现有布局钩子的外层类名。 */
  containerClassName: string;
  /** 搜索图标尺寸；默认与紧凑工具栏一致。 */
  iconSize?: number;
}

/** 复用资源树、轨迹台账和分支菜单一致的搜索输入结构。 */
export function SearchField({
  containerClassName,
  iconSize = 14,
  ...inputProps
}: SearchFieldProps) {
  return (
    <div className={containerClassName}>
      <IconSearch size={iconSize} />
      <Input {...inputProps} />
    </div>
  );
}
