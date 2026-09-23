import {
  Tabs as AppicaTabs,
  TabsList as AppicaTabsList,
  TabsTrigger as AppicaTabsTrigger,
  TabsContent as AppicaTabsContent,
  type TabsProps as AppicaTabsProps,
  type TabsListProps as AppicaTabsListProps,
  type TabsTriggerProps as AppicaTabsTriggerProps,
  type TabsContentProps as AppicaTabsContentProps,
} from "@appica/ui-react/tabs";

import { cn } from "@/lib/utils";

/**
 * 共享 Tabs 入口。variant/size/orientation 由官方在根节点通过 context 下传，
 * 这里锁定 size="md" 并保留必要的布局类，不再覆盖官方几何
 * （列表内边距、触发器内边距与活动指示器均由官方提供）。
 */
export function Tabs({ className, ...props }: Omit<AppicaTabsProps, "size">) {
  return (
    <AppicaTabs
      size="md"
      className={cn("group/tabs flex data-[orientation=horizontal]:flex-col", className)}
      {...props}
    />
  );
}

export function TabsList({ className, ...props }: Omit<AppicaTabsListProps, "size">) {
  return <AppicaTabsList size="md" className={cn(className)} {...props} />;
}

export function TabsTrigger({ className, ...props }: Omit<AppicaTabsTriggerProps, "size">) {
  return (
    <AppicaTabsTrigger
      size="md"
      className={cn("flex-1 text-ui-base", className)}
      {...props}
    />
  );
}

export function TabsContent({ className, ...props }: AppicaTabsContentProps) {
  return <AppicaTabsContent className={cn("flex-1 text-sm outline-none", className)} {...props} />;
}
