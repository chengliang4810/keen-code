import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tip } from "@/components/ui/tooltip";
import { IconArrowsSort, IconClock, IconMessageCircle } from "@/components/icons";
import type { SidebarSortMode } from "@/lib/sidebarOrder";
import type { SidebarTranslator } from "./types";

export interface SidebarSortMenuProps {
  tr: SidebarTranslator;
  mode: SidebarSortMode;
  onModeChange: (mode: SidebarSortMode) => void;
}

/** 项目分组标题右侧的会话排序方式选择。 */
export function SidebarSortMenu({
  tr,
  mode,
  onModeChange,
}: SidebarSortMenuProps) {
  return (
    <DropdownMenu>
      <Tip label={tr("sidebar.sort")}>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            className="tree-l1__action"
            aria-label={tr("sidebar.sort")}
          >
            <IconArrowsSort size={15} />
          </Button>
        </DropdownMenuTrigger>
      </Tip>
      <DropdownMenuContent
        className="cmm__dropdown-content w-52"
        align="end"
        sideOffset={6}
      >
        <DropdownMenuLabel>{tr("sidebar.sort")}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={mode}
          onValueChange={(value) => onModeChange(value as SidebarSortMode)}
        >
          <DropdownMenuRadioItem value="lastUserMessage">
            <IconMessageCircle size={15} />
            <span>{tr("sidebar.sortByLastUserMessage")}</span>
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="updatedAt">
            <IconClock size={15} />
            <span>{tr("sidebar.sortByUpdatedAt")}</span>
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
