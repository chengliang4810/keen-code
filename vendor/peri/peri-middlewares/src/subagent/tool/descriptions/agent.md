Launch a sub-agent with an independent context to handle a specialized sub-task. A KeenCode project sub-agent executes from `.keencode/agents/{subagent_type}.md`; the filename ID must exactly match its frontmatter `name`. Built-in and plugin agents remain in their own catalogs.

Fork mode (fork: true):
- Inherits the parent's frozen system prompt, a full history snapshot at launch time, and the parent's core tool set (Filesystem, Bash, Web, MCP)
- Does NOT inherit the Agent tool (prevents recursion) nor Cron / LSP / Plugin extension tools; parent agent_overrides blocks do not enter the forked prompt
- The prompt is treated as a directive within the existing context, not a standalone briefing
- Do NOT re-explain background that is already in the conversation history
- Use for tasks that require context from the ongoing conversation (e.g., continuing a multi-file refactor)
- The forked agent follows a structured output format: Scope, Result, Key files, Files changed

Usage:
- Provide a clear, self-contained task description via the prompt parameter. The sub-agent has no access to the parent conversation history
- **subagent_type is REQUIRED for NEW sub-agents** unless fork=true. Specify an agent ID matching an existing agent definition file. Do NOT omit this parameter unless you intend to fork the current agent — **or resume** (when `resume_thread_id` is provided, `subagent_type` and `fork` are ignored: resume takes priority)
- The sub-agent inherits the parent's tool set by default, excluding Agent itself (to prevent recursion)
- **Authorization boundary**: approving the `Agent` tool grants the sub-agent the right to execute its inherited tools. Sub-agents do NOT run per-tool HITL approval — internal tool calls (Bash, Write, Edit, WebFetch, MCP, ...) execute without further approval prompts. The transfer is single-level: sub-agents cannot recursively launch further sub-agents
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
- **When an Agent call returns an interrupted/error message or a background notification contains `child_thread_id: xxx (resume with Agent(resume_thread_id: xxx))` and the task still needs to be completed, resume the execution with `Agent(resume_thread_id: xxx)` instead of launching a new sub-agent** — this avoids repeating work already done and losing side effects

Return format:
- If the sub-agent made tool calls, the result includes a summary of tools used followed by the final response
- If no tool calls were made, only the final response text is returned

Background execution (run_in_background: true):
- Runs the sub-agent asynchronously while the main agent continues immediately.
- Maximum 3 concurrent background tasks.
- The main agent will be notified when the task completes via a system message.
- **Only use when you genuinely need to continue working while the sub-agent runs** (e.g., offloading a long-running code review while you proceed with other edits). For most cases, run sub-agents synchronously to integrate their results immediately.

Resume execution (resume_thread_id):
- Resume an interrupted sub-agent from its persisted thread: the execution state (transcript) is replayed from disk and execution continues — **no new sub-agent is created**
- The thread must not be active: interrupted or failed threads can be resumed; threads left active by a crash require manual handling
- Takes priority over `subagent_type` and `fork`: when `resume_thread_id` is provided, those fields are ignored (no error); `prompt` is optional — when omitted, the sub-agent implicitly continues where it left off, and you may also provide new instructions to adjust direction
- Can be combined with `run_in_background: true` (the resumed execution follows that mode)
- **Common failures**: (1) passing `subagent_type` or `fork` together with `resume_thread_id` — harmless, they are ignored; resume always wins. (2) `parent thread mismatch` → the thread belongs to another parent agent (e.g. a sibling spawned in parallel); only its original parent can resume it — otherwise spawn a new sub-agent with `subagent_type`. (3) `thread not found` / `invalid thread id` → the id is stale or malformed; use the `child_thread_id` exactly as returned in the interrupted/error/bg notification text.
