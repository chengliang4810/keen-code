Wait for background Agent state to change while the current main-agent turn remains active.

- Use this only when your next step depends on an Agent result.
- Continue independent work instead of waiting when useful work remains.
- The wait ends on an Agent state change, new user input, main-turn cancellation, timeout, or immediately when no Agent is running.
- Only Agent tasks are observed. Background Shell changes do not wake this tool.
- Completed Agent output is delivered separately through AgentResult; this tool returns only the outcome and currently running task/thread identifiers.
- The default timeout is 30 seconds. You may wait again after a timeout.
- Do not use Shell, sleep, or polling as a substitute for WaitAgent.
