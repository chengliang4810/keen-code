Launch an asynchronous sub-agent with an independent context to handle a specialized sub-task. Agent, Fork, and Resume always return immediately with task and thread identifiers. A KeenCode project sub-agent executes from `.keencode/agents/{subagent_type}.md`; the filename ID must exactly match its frontmatter `name`. Built-in and plugin agents remain in their own catalogs.

Fork mode (fork: true):
- Inherits the parent's frozen system prompt, a full history snapshot at launch time, and the parent's core tool set (Filesystem, Bash, Web, MCP)
- Does NOT inherit the Agent tool (prevents recursion) nor Cron / LSP / Plugin extension tools; parent agent_overrides blocks do not enter the forked prompt
- The prompt is treated as a directive within the existing context, not a standalone briefing
- Do NOT re-explain background that is already in the conversation history
- Use for tasks that require context from the ongoing conversation (e.g., continuing a multi-file refactor)
- The forked agent follows a structured output format: Scope, Result, Key files, Files changed

Usage:
- For a defined-type sub-agent, provide a clear, self-contained task description via the prompt parameter. It has no access to the parent conversation history
- **subagent_type is REQUIRED for NEW sub-agents** unless fork=true. Specify an agent ID matching an existing agent definition file. Do NOT omit this parameter unless you intend to fork the current agent — **or resume** (when `resume_thread_id` is provided, `subagent_type` and `fork` are ignored: resume takes priority)
- The sub-agent inherits the parent's tool set by default, excluding Agent itself (to prevent recursion)
- **Execution boundary**: calling the `Agent` tool delegates the inherited tool set to the sub-agent. Internal tool calls (Bash, Write, Edit, WebFetch, MCP, ...) execute directly under the same project scope. The transfer is single-level: sub-agents cannot recursively launch further sub-agents
- Agent definitions may restrict available tools via the tools and disallowedTools fields in frontmatter
- The sub-agent executes in isolated state — it cannot access the parent's message history or intermediate results

Model selection:
- NEW defined-type sub-agents use the optional `model` declared in the agent definition frontmatter; when present it must be `provider_id::model`
- When the definition omits `model`, the sub-agent follows the current session model
- Built-in model overrides from Settings use the same `provider_id::model` format; when no override is present, the built-in agent follows the current session model
- The Agent tool has no call-time model override; forks inherit the parent model and resumes restore the original execution context

When to use:
- For tasks that benefit from independent context isolation (e.g., code review while working on a different feature)
- For tasks requiring specialized persona or behavior defined in agent configuration files
- For parallelizable sub-tasks that do not depend on each other's results
- When you need to break a complex task into smaller, independently executable pieces
- Use `followup_task(target: child_thread_id, message: ...)` to append or adjust work without replacing the Agent thread. Running Agents receive it at the next message boundary or after the pending tool call; idle Agents start a new turn on the same thread
- Use `interrupt_agent(target: child_thread_id)` to stop only the current Agent turn. It returns the previous status and keeps the thread available for a later `followup_task`

Return format:
- Agent, Fork, and Resume immediately return `task_id` and `child_thread_id`; they do not wait for the sub-agent body
- Completion output is delivered later through an `AgentResult` message; `AgentResult` is not a polling tool

Asynchronous orchestration:
- All Agent calls are asynchronous; there is no `run_in_background` parameter or synchronous fallback
- The Agent limit is user-configurable in Settings (default 10, maximum 999). Background Shell tasks use a separate fixed limit of 5
- Continue useful independent work after launch
- When your next step depends on a sub-agent result, call `WaitAgent`; after a timeout you may call it again
- Do not use Shell, sleep, or polling loops to wait for Agent results
- If the result is not needed, you may finish the main turn without waiting
- Avoid editing the same files while a `[writes]` Agent is running

Resume execution (resume_thread_id):
- Explicitly resume an interrupted sub-agent from its persisted thread when calling `Agent` directly; for normal continuation or task adjustment, prefer `followup_task`. The execution state (transcript) is replayed from disk and execution continues — **no new sub-agent is created**
- The thread must not be active: interrupted or failed threads can be resumed; threads left active by a crash require manual handling
- Takes priority over `subagent_type` and `fork`: when `resume_thread_id` is provided, those fields are ignored (no error); `prompt` is optional — when omitted, the sub-agent implicitly continues where it left off, and you may also provide new instructions to adjust direction
- **Common failures**: (1) passing `subagent_type` or `fork` together with `resume_thread_id` — harmless, they are ignored; resume always wins. (2) `parent thread mismatch` → the thread belongs to another parent agent (e.g. a sibling spawned in parallel); only its original parent can resume it — otherwise spawn a new sub-agent with `subagent_type`. (3) `thread not found` / `invalid thread id` → the id is stale or malformed; use the `child_thread_id` exactly as returned in the interruption, error, or background notification text.
