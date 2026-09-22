# Prompt source

Source repository: https://github.com/Gitlawb/openclaude
Pinned commit: `1afeb4b1e7a892b6456c6b86f6fd6683222b2005` (checked 2026-09-07).

The complete built-in registry is `src/tools/AgentTool/builtInAgents.ts`: general-purpose, statusline-setup, code-reviewer, Explore, Plan, claude-code-guide, verification. Per the user's explicit product scope, statusline-setup and claude-code-guide are excluded. Coordinator mode is a separate workflow, outside KeenCode's single-level delegation scope.

Imported from `src/tools/AgentTool/built-in/{generalPurposeAgent,exploreAgent,planAgent,verificationAgent,codeReviewerAgent}.ts`. Both system prompts and whenToUse descriptions are copied; the exact upstream copyright and license notice is retained in `UPSTREAM-LICENSE.txt`. The upstream license distinguishes derived Anthropic code from contributor modifications; this directory is not represented as wholly MIT-licensed.

Adaptations:
- Resolve interpolation using the non-embedded search branch and KeenCode's existing Read/Glob/Grep/Bash/WebFetch names.
- Replace host identity with KeenCode, keep stable lowercase agent IDs, and inherit the user's configured provider/model instead of vendor model aliases.
- Explore, Plan and code-reviewer use an explicit Read/Glob/Grep allow-list. Remove unavailable Bash advice from Explore/Plan. Retain code-reviewer's inline-diff requirement and shell prohibition.
- Map the reviewer's routing example to `spawn_agent` with the `agent` field. Child delegation remains disabled by the runtime; no nested orchestration, vendor feature flags or telemetry are imported.
- Verification retains its runtime-testing role and required verdict. No account-specific or UI-specific runtime fields are imported.
