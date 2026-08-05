/**
 * App icons — Tabler Icons only (https://tabler.io/icons).
 * Stable `Icon*` names for call sites. No other icon libraries / local SVG packs.
 */

import type { ComponentType } from "react";
import {
  IconActivity as TbActivity,
  IconAlertTriangle as TbAlertTriangle,
  IconArchive as TbArchive,
  IconArrowLeft as TbArrowLeft,
  IconArrowsMinimize as TbArrowsMinimize,
  IconBlockquote as TbBlockquote,
  IconBold as TbBold,
  IconBolt as TbBolt,
  IconGitBranch as TbGitBranch,
  IconGitCommit as TbGitCommit,
  IconBox as TbBox,
  IconBrush as TbBrush,
  IconCheck as TbCheck,
  IconCircle as TbCircle,
  IconCircleCheck as TbCircleCheck,
  IconClock as TbClock,
  IconCode as TbCode,
  IconChevronDown as TbChevronDown,
  IconChevronLeft as TbChevronLeft,
  IconChevronRight as TbChevronRight,
  IconCircleDashed as TbCircleDashed,
  IconCopy as TbCopy,
  IconDots as TbDots,
  IconCrop as TbCrop,
  IconDownload as TbDownload,
  IconEdit as TbEdit,
  IconH1 as TbH1,
  IconH2 as TbH2,
  IconH3 as TbH3,
  IconItalic as TbItalic,
  IconFileDiff as TbFileDiff,
  IconFileText as TbFileText,
  IconFiles as TbFiles,
  IconFirstAidKit as TbFirstAidKit,
  IconFolder as TbFolder,
  IconFolderPlus as TbFolderPlus,
  IconInfoCircle as TbInfoCircle,
  IconExternalLink as TbExternalLink,
  IconLayoutSidebar as TbLayoutSidebar,
  IconLayoutSidebarRight as TbLayoutSidebarRight,
  IconLink as TbLink,
  IconList as TbList,
  IconListNumbers as TbListNumbers,
  IconListTree as TbListTree,
  IconListDetails as TbListDetails,
  IconLoader2 as TbLoader2,
  IconMarkdown as TbMarkdown,
  IconMinus as TbMinus,
  IconPaperclip as TbPaperclip,
  IconPencil as TbPencil,
  IconPinned as TbPinned,
  IconPinnedOff as TbPinnedOff,
  IconPlayerStop as TbPlayerStop,
  IconPlayerPause as TbPlayerPause,
  IconPlug as TbPlug,
  IconPlus as TbPlus,
  IconPuzzle as TbPuzzle,
  IconRefresh as TbRefresh,
  IconRobot as TbRobot,
  IconSearch as TbSearch,
  IconSend as TbSend,
  IconSeparator as TbSeparator,
  IconSettings as TbSettings,
  IconSquare as TbSquare,
  IconStack2 as TbStack2,
  IconStrikethrough as TbStrikethrough,
  IconTarget as TbTarget,
  IconTool as TbTool,
  IconTrash as TbTrash,
  IconUpload as TbUpload,
  IconUser as TbUser,
  IconX as TbX,
} from "@tabler/icons-react";

export type IconProps = {
  size?: number;
  title?: string;
  className?: string;
  stroke?: number;
};

type TbIcon = ComponentType<{
  size?: number | string;
  stroke?: number;
  color?: string;
  className?: string;
  "aria-hidden"?: boolean | "true" | "false";
}>;

function wrap(Tb: TbIcon, defaults?: { stroke?: number; className?: string }) {
  function TablerAppIcon({
    size = 18,
    title,
    stroke = defaults?.stroke ?? 1.75,
    className = "",
  }: IconProps) {
    const classes = ["g-icon", defaults?.className, className]
      .filter(Boolean)
      .join(" ");
    return (
      <span
        className={classes}
        style={{
          display: "inline-flex",
          width: size,
          height: size,
          lineHeight: 0,
          color: "currentColor",
          flexShrink: 0,
          alignItems: "center",
          justifyContent: "center",
        }}
        role={title ? "img" : undefined}
        aria-hidden={title ? undefined : true}
        aria-label={title}
        title={title}
      >
        <Tb size={size} stroke={stroke} color="currentColor" aria-hidden />
      </span>
    );
  }
  return TablerAppIcon;
}

export const IconSearch = wrap(TbSearch);
/** New chat / compose — Tabler Edit (pencil writing on paper). */
export const IconNewChat = wrap(TbEdit);
export const IconEdit = wrap(TbEdit);
/** Markdown / TipTap format toolbar */
export const IconBold = wrap(TbBold);
export const IconItalic = wrap(TbItalic);
export const IconStrikethrough = wrap(TbStrikethrough);
export const IconCode = wrap(TbCode);
export const IconH1 = wrap(TbH1);
export const IconH2 = wrap(TbH2);
export const IconH3 = wrap(TbH3);
export const IconListNumbers = wrap(TbListNumbers);
export const IconBlockquote = wrap(TbBlockquote);
export const IconSeparator = wrap(TbSeparator);
/** Wallpaper focus / crop frame editor. */
export const IconCrop = wrap(TbCrop);
export const IconClock = wrap(TbClock);
export const IconSkills = wrap(TbTool);
export const IconChevronDown = wrap(TbChevronDown);
export const IconChevronLeft = wrap(TbChevronLeft);
export const IconChevronRight = wrap(TbChevronRight);
export const IconFolderPlus = wrap(TbFolderPlus);
export const IconPlus = wrap(TbPlus);
export const IconMore = wrap(TbDots);
export const IconFolder = wrap(TbFolder);
export const IconRename = wrap(TbPencil);
export const IconLink = wrap(TbLink);
export const IconTrash = wrap(TbTrash, { className: "g-icon--danger" });
export const IconPaperclip = wrap(TbPaperclip);
export const IconAttach = wrap(TbPaperclip);
export const IconClose = wrap(TbX);
export const IconSend = wrap(TbSend);
export const IconQueue = wrap(TbStack2);
export const IconPanel = wrap(TbLayoutSidebar);
/** Right files / context pane (Codex-style top bar). */
export const IconPanelRight = wrap(TbLayoutSidebarRight);
/** Open project in Finder / external app. */
export const IconExternalLink = wrap(TbExternalLink);
export const IconList = wrap(TbList);
export const IconSettings = wrap(TbSettings);
export const IconDoctor = wrap(TbFirstAidKit);
export const IconStop = wrap(TbPlayerStop);
/** 目标状态管理入口。 */
export const IconPause = wrap(TbPlayerPause);
export const IconHistory = wrap(TbRefresh);
/** Session fork / branch. */
export const IconFork = wrap(TbGitBranch);
/** Git branch indicator (composer context bar). */
export const IconGitBranch = wrap(TbGitBranch);
export const IconFiles = wrap(TbFiles);
/** Session changes / diff panel (resource viewer). */
export const IconFileDiff = wrap(TbFileDiff);
/** File tree panel toggle (resource viewer). */
export const IconListTree = wrap(TbListTree);
export const IconRefresh = wrap(TbRefresh);
export const IconCopy = wrap(TbCopy);
export const IconDownload = wrap(TbDownload);
export const IconExportMd = wrap(TbMarkdown);
export const IconArchive = wrap(TbArchive);
export const IconFileText = wrap(TbFileText);
export const IconBolt = wrap(TbBolt);
export const IconMinimize = wrap(TbMinus);
export const IconMaximize = wrap(TbSquare);
export const IconPin = wrap(TbPinned);
export const IconPinOff = wrap(TbPinnedOff);
export const IconAlertTriangle = wrap(TbAlertTriangle);
export const IconCheck = wrap(TbCheck);
/** 计划中尚未开始的步骤。 */
export const IconCircle = wrap(TbCircle);
/** 计划中已经完成的步骤。 */
export const IconCircleCheck = wrap(TbCircleCheck);
/** 计划中正在执行的步骤与总进度。 */
export const IconLoader = wrap(TbLoader2);
export const IconArrowLeft = wrap(TbArrowLeft);
export const IconUser = wrap(TbUser);
export const IconAppearance = wrap(TbBrush);
export const IconInfo = wrap(TbInfoCircle);
/** Slash palette / goal mode */
export const IconTarget = wrap(TbTarget);
export const IconArrowsMinimize = wrap(TbArrowsMinimize);

/**
 * Two chevrons facing each other (∨ above ∧) — collapse all project folders.
 * Glyph is slightly inset with a clearer mid gap; stroke stays Tabler 1.75
 * so weight matches IconPlus at the same box size.
 */
export function IconArrowsVerticalCollapse({
  size = 15,
  title,
  stroke = 1.75,
  className = "",
}: IconProps) {
  const classes = ["g-icon", className].filter(Boolean).join(" ");
  return (
    <span
      className={classes}
      style={{
        display: "inline-flex",
        width: size,
        height: size,
        lineHeight: 0,
        color: "currentColor",
        flexShrink: 0,
        alignItems: "center",
        justifyContent: "center",
      }}
      role={title ? "img" : undefined}
      aria-hidden={title ? undefined : true}
      aria-label={title}
      title={title}
    >
      <svg
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="none"
        xmlns="http://www.w3.org/2000/svg"
        aria-hidden
      >
        {/* Upper chevron: ∨ — smaller, higher */}
        <path
          d="M8.5 7L12 10.25L15.5 7"
          stroke="currentColor"
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        {/* Lower chevron: ∧ — smaller, lower (wider mid gap) */}
        <path
          d="M8.5 17L12 13.75L15.5 17"
          stroke="currentColor"
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </span>
  );
}
export const IconCircleDashed = wrap(TbCircleDashed);
export const IconPlug = wrap(TbPlug);
export const IconActivity = wrap(TbActivity);
export const IconBox = wrap(TbBox);
export const IconPuzzle = wrap(TbPuzzle);
/** 会话摘要概览。 */
export const IconSummary = wrap(TbListDetails);
/** Git 提交记录。 */
export const IconGitCommit = wrap(TbGitCommit);
/** 子智能体。 */
export const IconSubagent = wrap(TbRobot);
/** 推送到远端。 */
export const IconPush = wrap(TbUpload);
