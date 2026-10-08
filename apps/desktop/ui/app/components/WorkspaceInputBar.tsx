import { useTranslation } from "@/modules/i18n";
import { homeRelativePath } from "@/lib/homeRelativePath";
import { cn } from "@/lib/utils";
import { AiInputBarConnect } from "@/modules/ai";
import { AiStatusBarControls } from "@/modules/ai/components/AiStatusBarControls";
import { Chip } from "@/modules/ai/components/Chip";
import { ChipsRow } from "@/modules/ai/components/ChipsRow";
import { useComposer } from "@/modules/ai/lib/composer";
import { useBlockController } from "@/modules/terminal/lib/blockController";
import { focusLeafInput } from "@/modules/terminal/lib/terminalSessionApi";
import {
  AiContentGenerator02Icon,
  CommandLineIcon,
  Folder01Icon,
  GitBranchIcon,
  TerminalIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { OsIcon } from "./OsIcon";
import { useGitBranch } from "./useGitBranch";
import { useSystemInfo } from "./useSystemInfo";

const ShellInput = lazy(() => import("@/modules/terminal/block/ShellInput"));
const AiComposerInput = lazy(() =>
  import("@/modules/ai/components/AiComposerInput").then((m) => ({
    default: m.AiComposerInput,
  })),
);

export const TOGGLE_BLOCK_INPUT_EVENT = "rcode:toggle-block-input";

type Props = {
  isBlockTab: boolean;
  isTerminalTab: boolean;
  activeLeafId: number | null;
  cwd: string | null;
  home: string | null;
  hasComposer: boolean;
  panelOpen: boolean;
  keysLoaded: boolean;
  onConnect: () => void;
  agentWorkbench?: boolean;
  /** 起始页复用同一输入能力，外层卡片负责边框和底色。 */
  standalone?: boolean;
};

export function WorkspaceInputBar({
  isBlockTab,
  isTerminalTab,
  activeLeafId,
  cwd,
  home,
  hasComposer,
  panelOpen,
  keysLoaded,
  onConnect,
  agentWorkbench = false,
  standalone = false,
}: Props) {
  const tr = useTranslation();
  const c = useComposer();
  const { os, shell } = useSystemInfo();

  const controller = useBlockController(isBlockTab ? activeLeafId : null);
  const blockMode = controller?.blockMode ?? "prompt";

  // Re-resolve the branch chip when a command finishes (covers `git checkout`).
  const [promptNonce, setPromptNonce] = useState(0);
  const prevBlockMode = useRef(blockMode);
  useEffect(() => {
    if (prevBlockMode.current !== "prompt" && blockMode === "prompt") {
      setPromptNonce((n) => n + 1);
    }
    prevBlockMode.current = blockMode;
  }, [blockMode]);
  const branch = useGitBranch(isTerminalTab ? cwd : null, promptNonce);

  const showToggle = isBlockTab && hasComposer;
  const [mode, setMode] = useState<"shell" | "ai">("shell");
  const effectiveMode = !isBlockTab ? "ai" : hasComposer ? mode : "shell";

  const mounted = keysLoaded || isBlockTab || agentWorkbench;
  const open = agentWorkbench || isBlockTab || (keysLoaded && panelOpen);

  const [aiLoaded, setAiLoaded] = useState(false);
  useEffect(() => {
    if (open && effectiveMode === "ai") setAiLoaded(true);
  }, [open, effectiveMode]);
  const renderAi = (hasComposer || agentWorkbench) && aiLoaded;

  const switchMode = (next: "shell" | "ai") => {
    setMode(next);
    requestAnimationFrame(() => {
      if (next === "ai") c.textareaRef.current?.focus();
      else if (activeLeafId != null) focusLeafInput(activeLeafId);
    });
  };

  const modeRef = useRef(mode);
  modeRef.current = mode;
  const switchModeRef = useRef(switchMode);
  switchModeRef.current = switchMode;
  useEffect(() => {
    if (!showToggle) return;
    const onToggle = () =>
      switchModeRef.current(modeRef.current === "shell" ? "ai" : "shell");
    window.addEventListener(TOGGLE_BLOCK_INPUT_EVENT, onToggle);
    return () => window.removeEventListener(TOGGLE_BLOCK_INPUT_EVENT, onToggle);
  }, [showToggle]);

  if (!mounted) return null;

  const terminalChips = isTerminalTab ? (
    <>
      {os && <Chip tone="neutral" iconNode={<OsIcon os={os} />} title={os} />}
      {cwd && (
        <Chip tone="blue" icon={Folder01Icon} title={cwd}>
          {homeRelativePath(cwd, home)}
        </Chip>
      )}
      {branch && (
        <Chip
          tone="violet"
          icon={GitBranchIcon}
          title={tr("Branch: {value0}", { value0: branch })}
        >
          {branch}
        </Chip>
      )}
      {shell && (
        <Chip tone="emerald" icon={CommandLineIcon}>
          {shell}
        </Chip>
      )}
    </>
  ) : null;

  const content =
    !hasComposer && !isBlockTab && !agentWorkbench ? (
      <AiInputBarConnect onAdd={onConnect} />
    ) : (
      <div
        data-composer=""
        className={cn(
          "shrink-0 px-3 py-2",
          !standalone &&
            !agentWorkbench &&
            "border-t border-border/60 bg-foreground/[0.02]",
          agentWorkbench &&
            !standalone &&
            "rounded-2xl border border-border bg-card shadow-sm",
        )}
      >
        <div className="flex flex-col gap-2 rounded-lg px-1 py-1">
          <ChipsRow
            leading={terminalChips}
            files={c.files}
            onRemoveFile={c.removeFile}
            commands={c.pickedCommands}
            onRemoveCommand={(name) => c.removeCommand(name)}
          />

          <div className="flex items-end gap-2.5">
            <div className="relative min-w-0 flex-1">
              {isBlockTab && controller && activeLeafId != null && (
                <div className={cn(effectiveMode !== "shell" && "hidden")}>
                  {blockMode === "plain" ? (
                    <button
                      type="button"
                      className="text-ui-base text-muted-foreground hover:text-foreground"
                      onClick={() => focusLeafInput(activeLeafId)}
                    >
                      {tr("Type directly in the terminal")}
                    </button>
                  ) : (
                    <Suspense fallback={null}>
                      <ShellInput
                        leafId={activeLeafId}
                        mode={blockMode}
                        focused={effectiveMode === "shell"}
                        onSubmit={controller.submitCommand}
                        onInterrupt={controller.interrupt}
                        getCwd={controller.getCwd}
                      />
                    </Suspense>
                  )}
                </div>
              )}
              {renderAi && (
                <div className={cn(effectiveMode !== "ai" && "hidden")}>
                  <Suspense fallback={null}>
                    <AiComposerInput spacious={agentWorkbench} />
                  </Suspense>
                </div>
              )}
            </div>
            {showToggle && (
              <div className="shrink-0 pb-px">
                <ModeToggle mode={mode} onChange={switchMode} />
              </div>
            )}
          </div>
          {agentWorkbench && (
            <div className="flex justify-end">
              <AiStatusBarControls embedded spread />
            </div>
          )}
        </div>
      </div>
    );

  return (
    <div
      data-ai-input-bar
      data-state={open ? "open" : "closed"}
      className="rcode-reveal"
      aria-hidden={!open}
    >
      <div>{content}</div>
    </div>
  );
}

function ModeToggle({
  mode,
  onChange,
}: {
  mode: "shell" | "ai";
  onChange: (next: "shell" | "ai") => void;
}) {
  const tr = useTranslation();
  return (
    <div className="relative grid shrink-0 grid-cols-2 rounded-md p-0.5 text-ui-base ring-1 ring-inset ring-border/35">
      <span
        aria-hidden
        className="pointer-events-none absolute inset-y-0.5 left-0.5 w-[calc(50%-2px)] rounded-[4px] bg-accent/60 transition-transform duration-200 ease-out"
        style={{
          transform: mode === "ai" ? "translateX(100%)" : "translateX(0)",
        }}
      />
      <SegButton
        active={mode === "shell"}
        icon={TerminalIcon}
        label={tr("Shell")}
        onClick={() => onChange("shell")}
      />
      <SegButton
        active={mode === "ai"}
        icon={AiContentGenerator02Icon}
        label={tr("AI")}
        onClick={() => onChange("ai")}
      />
    </div>
  );
}

function SegButton({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: typeof AiContentGenerator02Icon;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "relative z-10 flex items-center justify-center gap-1 rounded-[4px] px-2 py-[2.5px] font-medium transition-colors",
        active
          ? "text-foreground/90"
          : "text-muted-foreground/70 hover:text-foreground",
      )}
    >
      <HugeiconsIcon icon={icon} size={11} strokeWidth={1.75} />
      {label}
    </button>
  );
}
