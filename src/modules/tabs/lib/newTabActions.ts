import {
  AiBrowserIcon,
  ComputerTerminal02Icon,
  FolderGitTwoIcon,
  FolderTreeIcon,
  GitBranchIcon,
  Globe02Icon,
} from "@hugeicons/core-free-icons";

/** 新增菜单与空侧栏共用入口定义，保持名称、顺序和图标一致。 */
export const NEW_TAB_ACTIONS = [
  { id: "files", label: "Files", icon: FolderTreeIcon },
  { id: "git", label: "Git", icon: FolderGitTwoIcon },
  { id: "terminal", label: "Terminal", icon: ComputerTerminal02Icon },
  { id: "agents", label: "Agents", icon: AiBrowserIcon },
  { id: "preview", label: "Preview", icon: Globe02Icon },
  { id: "gitGraph", label: "Git Graph", icon: GitBranchIcon },
] as const;

export type NewTabActionId = (typeof NEW_TAB_ACTIONS)[number]["id"];
