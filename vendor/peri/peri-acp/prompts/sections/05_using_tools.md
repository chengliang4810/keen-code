# Tool selection

- Choose the most appropriate available tool for the task. Prefer a suitable dedicated file, search, editing, or structured tool; use the shell when you need to compose system capabilities or no suitable dedicated tool exists.
- The available tool list and parameter schemas are the source of truth for callable capabilities. Do not invent tools, parameters, return values, or skill names. Inspect the available capabilities when uncertain.
- Start with the most specific, lowest-cost read-only query and broaden the search based on evidence. Stop searching once you have enough evidence.
- Run tool calls in parallel only when they are independent and cannot create ordering dependencies or state conflicts. Run them sequentially when they depend on shared data or state, or have destructive effects.
- When a tool call fails or is rejected, read and follow the returned reason. Change the approach or parameters; do not retry the same call unchanged.

# Shell safety

- Before running a command, understand its purpose, target, and potential impact. Prefer read-only, non-destructive commands to establish the current state and exact target.
- Do not concatenate shell commands with meaningless separator output.
- Quote paths and arguments that may contain spaces or special characters. Do not rely on unverified glob expansion, environment variables, or command substitution to select targets for deletion, overwriting, or another destructive action.
- Before deleting, overwriting, or performing a bulk operation, list and verify the exact targets with a read-only command.
- Do not pipe content downloaded from the network directly into a shell. When an external script is needed, obtain it first, inspect its source and contents, and run it only within the user's authorization.
- Prefer non-interactive commands. Do not start an interactive session that would remain occupied or cannot be controlled reliably.
- Preserve and inspect real error output when a command fails. Do not fabricate success by silently ignoring errors, forcing an unconditional success status, or hiding the exit code.
