import type { UIMessage } from "@ai-sdk/react";
import type { SessionMeta } from "@/modules/ai/lib/sessions";
import type {
  AgentResource,
  AgentResources,
  InstalledPlugin,
} from "@/modules/ai/lib/extensions";

export const SESSION_CONTEXT_LIMIT = 32_000;

export function contextSessions(
  sessions: readonly SessionMeta[],
  active: SessionMeta | undefined,
): SessionMeta[] {
  if (!active) return [];
  return sessions
    .filter((session) => {
      if (session.id === active.id || session.archived) return false;
      if (
        (session.workspaceScope ?? "local") !==
        (active.workspaceScope ?? "local")
      )
        return false;
      if (active.projectless) return session.projectless === true;
      return !!active.projectId && session.projectId === active.projectId;
    })
    .sort((a, b) => b.updatedAt - a.updatedAt);
}

export function sessionContext(messages: readonly UIMessage[]): string {
  const blocks: string[] = [];
  let remaining = SESSION_CONTEXT_LIMIT;
  for (let i = messages.length - 1; i >= 0 && remaining > 0; i--) {
    const message = messages[i];
    if (message.role !== "user" && message.role !== "assistant") continue;
    const parts: string[] = [];
    let available = remaining;
    for (const part of message.parts) {
      if (part.type !== "text" || !part.text.trim() || available <= 0) continue;
      const text = part.text.slice(0, available);
      parts.push(text);
      available -= text.length + 1;
    }
    if (!parts.length) continue;
    const block = `${message.role}: ${parts.join("\n")}`.slice(0, remaining);
    blocks.unshift(block);
    remaining -= block.length + 2;
  }
  return blocks.join("\n\n").slice(0, SESSION_CONTEXT_LIMIT);
}

export function extensionContext(
  resource: AgentResource,
  kind: "skill" | "command",
): string {
  return `The user selected this ${kind}. Load its instructions using the registered ${kind === "skill" ? "Skill" : "PluginCommand"} tool before applying it to the user's request. All normal permissions still apply.\n${JSON.stringify({ name: resource.name })}`;
}

export function pluginContext(
  plugin: InstalledPlugin,
  resources: AgentResources | null,
): string {
  const namespace = `plugin:${plugin.id.marketplace ?? "local"}:${plugin.id.plugin}`;
  return `The user selected this enabled plugin. Use its applicable registered capabilities for the request. Load command templates with PluginCommand and skill instructions with Skill. Use only available MCP tools belonging to this plugin. All normal permissions still apply.\n${JSON.stringify(
    {
      plugin: `${plugin.id.plugin}@${plugin.id.marketplace ?? "local"}`,
      commands:
        resources?.commands
          .filter(
            (resource) =>
              resource.enabled && resource.name.startsWith(`${namespace}:`),
          )
          .map((resource) => resource.name) ?? [],
      skills:
        resources?.skills
          .filter(
            (resource) =>
              resource.enabled &&
              resource.source === "plugin" &&
              resource.name.startsWith(`${plugin.id.plugin}:`),
          )
          .map((resource) => resource.name) ?? [],
    },
  )}`;
}

export function contextBlock(name: string, text: string): string {
  return `<context>\n${JSON.stringify({ name, content: text }).replace(/</g, "\\u003c").replace(/>/g, "\\u003e")}\n</context>`;
}
