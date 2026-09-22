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

export type TabsVariant = "default" | "line";
export type TabsSize = "sm" | "md" | "lg";
export type TabsTriggerSize = TabsSize | "icon-sm" | "icon-md" | "icon-lg";

export interface TabsProps extends Omit<AppicaTabsProps, "variant"> {
  variant?: TabsVariant;
}

export function Tabs({ className, variant = "default", size: _size = "md", orientation = "horizontal", ...props }: TabsProps) {
  return <AppicaTabs variant={variant === "default" ? "pill" : "line"} size="md" orientation={orientation} className={cn("group/tabs flex data-[orientation=horizontal]:flex-col", className)} {...props} />;
}

export interface TabsListProps extends Omit<AppicaTabsListProps, "variant"> {
  variant?: TabsVariant;
}

export function TabsList({ className, variant, size: _size, ...props }: TabsListProps) {
  return <AppicaTabsList variant={variant === "line" ? "line" : "pill"} size="md" className={cn("keencode-tabs-list-md inline-flex w-fit items-center justify-center rounded-lg p-[3px] text-foreground-subtle data-[orientation=horizontal]:h-8", variant === "line" ? "gap-1 bg-transparent rounded-none" : "bg-muted", className)} {...props} />;
}

export interface TabsTriggerProps extends Omit<AppicaTabsTriggerProps, "variant"> {
  variant?: TabsVariant;
  size?: TabsTriggerSize;
}

export function TabsTrigger({ className, variant, size: _size, ...props }: TabsTriggerProps) {
  return <AppicaTabsTrigger variant={variant === "line" ? "line" : "pill"} size="md" className={cn("group/trigger relative inline-flex h-[calc(100%-1px)] flex-1 items-stretch gap-1.5 rounded-md border border-transparent px-1.5 py-0.5 text-ui-base font-medium whitespace-nowrap text-foreground/60 transition-all hover:text-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-1 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-50 data-active:bg-background data-active:text-foreground group-data-[orientation=vertical]/tabs:w-full group-data-[orientation=vertical]/tabs:justify-start *:p-0", className)} {...props} />;
}

export function TabsContent({ className, ...props }: AppicaTabsContentProps) {
  return <AppicaTabsContent className={cn("flex-1 text-sm outline-none", className)} {...props} />;
}

export const tabsListVariants = (variant: TabsVariant = "default") => variant === "line" ? "gap-1 bg-transparent" : "bg-muted";
