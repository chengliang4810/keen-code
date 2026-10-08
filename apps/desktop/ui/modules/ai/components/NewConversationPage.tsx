import { WorkspaceInputBar } from "@/app/components/WorkspaceInputBar";
import { DraftGreeting } from "@/modules/ai/components/DraftGreeting";
import { ProjectPicker } from "@/modules/ai/components/ProjectPicker";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { useSpaces } from "@/modules/spaces";

type Props = {
  workspaceRoot: string | null;
  home: string | null;
  hasComposer: boolean;
  keysLoaded: boolean;
  onConnect: () => void;
  onSelectProject: (id: string | null) => void;
  onNewProject: () => void;
};

export function NewConversationPage({
  workspaceRoot,
  home,
  hasComposer,
  keysLoaded,
  onConnect,
  onSelectProject,
  onNewProject,
}: Props) {
  const projects = useSpaces((s) => s.spaces);
  const draft = useChatStore((s) => s.draftSession);
  const loading = useChatStore((s) => s.sessionLoading);

  return (
    <div
      className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]"
      data-new-conversation-page
    >
      <div className="flex min-h-full flex-col items-center px-4 before:block before:min-h-[52px] before:w-full before:basis-[29dvh] before:shrink before:content-[''] after:block after:min-h-4 after:w-full after:flex-1 after:content-['']">
        <div className="relative w-full max-w-2xl shrink-0">
          <DraftGreeting />
          <div
            data-draft-composer
            className="relative overflow-hidden rounded-2xl border border-border/60 bg-card shadow-sm"
          >
            <div className="flex min-w-0 items-center gap-2 p-1.5 pb-0">
              <ProjectPicker
                projects={projects}
                projectId={draft?.projectId}
                projectless={!!draft?.projectless}
                disabled={loading}
                onSelectProject={onSelectProject}
                onNewProject={onNewProject}
              />
            </div>
            <WorkspaceInputBar
              agentWorkbench
              standalone
              isBlockTab={false}
              isTerminalTab={false}
              activeLeafId={null}
              cwd={workspaceRoot}
              home={home}
              hasComposer={hasComposer}
              panelOpen
              keysLoaded={keysLoaded}
              onConnect={onConnect}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
