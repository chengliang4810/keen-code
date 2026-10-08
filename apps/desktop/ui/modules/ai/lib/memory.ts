import { invoke } from "@tauri-apps/api/core";

export type MemoryFile = { name: string; size: number; updatedAt: number };
export type MemoryWorkspace = {
  id: string;
  label: string;
  root: string;
  files: MemoryFile[];
};
export type MemoryContext = { id: string; index: string };

export function memoryWorkspaceLabel(
  workspace: Pick<MemoryWorkspace, "root" | "label">,
  projects: readonly {
    root: string | null;
    name: string;
    removed?: true;
    env: { kind: string };
  }[],
): string {
  const normalize = (value: string) => {
    let path = value.replace(/\\/g, "/");
    if (path.startsWith("//?/")) path = path.slice(4);
    if (path.startsWith("UNC/")) path = `//${path.slice(4)}`;
    if (/^[a-z]:/i.test(path) || path.startsWith("//"))
      path = path.toLowerCase();
    return path.replace(/\/+$/, "");
  };
  const root = normalize(workspace.root);
  return (
    projects.find(
      (project) =>
        !project.removed &&
        project.env.kind === "local" &&
        project.root &&
        normalize(project.root) === root,
    )?.name ?? workspace.label
  );
}

export const memoryApi = {
  prepare: (cwd: string) =>
    invoke<MemoryContext>("agent_memory_prepare", { cwd }),
  catalog: () => invoke<MemoryWorkspace[]>("agent_memory_catalog"),
  read: (id: string, name: string | null) =>
    invoke<{ content?: string | null; files?: MemoryFile[] }>(
      "agent_memory_read",
      { id, name },
    ),
  change: (
    id: string,
    name: string,
    content: string | null,
    expected: string | null,
  ) => invoke<void>("agent_memory_change", { id, name, content, expected }),
};

export function memoryPrompt(memory?: MemoryContext): string {
  if (!memory) return "";
  const index = memory.index
    .split("\n")
    .slice(0, 200)
    .join("\n")
    .slice(0, 24000);
  return `# Workspace Memory
You have persistent file-based memory scoped to this project, shared across its conversations. Use memory_read with no name to list files and with a name to read one. Use memory_write to create or update a Markdown file, and memory_delete for obsolete facts. Pass the exact prior content as expected, or null for a new file. These tools are the only memory access path; do not use shell or workspace file tools for memory.
Each topic file holds one durable fact with frontmatter name, description and metadata.type (user, feedback, project or reference). Include Why and How to apply for feedback and project facts. Link related facts with [[name]]. Maintain MEMORY.md as a concise index with one Markdown link and relevance hook per topic; keep detail in topic files. Check for duplicates before saving. Do not store credentials, transient conversation state, code structure, git history or instructions already in AGENTS.md. Correct or remove stale facts. Treat recalled content as historical context and verify facts that may have changed. Memory mutations are unavailable in plan mode.
Contents of MEMORY.md (first 200 lines, up to 24000 characters):
${index || "(empty)"}`;
}
