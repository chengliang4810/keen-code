import {
  isValidCommandName,
  parseCommandInvocation,
} from "@/modules/ai/lib/commands";
import type { SlashCommandMeta } from "@/modules/ai/lib/slashCommands";
import {
  extensionContext,
  contextBlock,
} from "@/modules/ai/lib/composerContext";

export async function commandPrompt(
  text: string,
  commands: readonly SlashCommandMeta[],
  resolve: (name: string, argumentsText: string) => Promise<string | null>,
): Promise<{ text: string; name: string | null }> {
  if (commands.length > 16) throw new Error("Too many commands.");
  const invocation = parseCommandInvocation(text);
  const prompts: string[] = [];
  let name: string | null = null;
  if (invocation && isValidCommandName(invocation.name)) {
    const prompt = await resolve(invocation.name, invocation.arguments);
    if (prompt !== null) {
      prompts.push(prompt);
      name = invocation.name;
    }
  }
  const seen = new Set(name ? [name.toLowerCase()] : []);
  for (const command of commands) {
    if (command.source !== "custom" || seen.has(command.name.toLowerCase()))
      continue;
    const prompt = await resolve(
      command.name,
      name ? (invocation?.arguments ?? text) : text,
    );
    if (prompt === null) throw new Error("Command file no longer exists.");
    prompts.push(prompt);
    seen.add(command.name.toLowerCase());
  }
  const plugins = commands
    .filter((command) => command.source === "plugin")
    .map((command) =>
      contextBlock(
        command.invocation,
        extensionContext(
          {
            name: command.name,
            description: command.label,
            source: "plugin",
            enabled: true,
            path: null,
          },
          "command",
        ),
      ),
    );
  return {
    text: [...plugins, ...(prompts.length ? prompts : [text])]
      .filter(Boolean)
      .join("\n\n"),
    name:
      name ??
      commands.find((command) => command.source === "custom")?.name ??
      null,
  };
}
