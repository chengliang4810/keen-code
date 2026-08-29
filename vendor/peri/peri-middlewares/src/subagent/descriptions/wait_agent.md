Wait for background Agent state to change while the current main-agent turn remains active.

- Use this only when your next step depends on an Agent result.
- Continue independent work instead of waiting when useful work remains.
- The wait ends on an Agent state change, new user input, main-turn cancellation, timeout, or immediately when no Agent is running.
- Only Agent tasks are observed. Background Shell changes do not wake this tool.
- When the wake is an Agent completion, the wait result carries the completed output inline in `results[]` (`message_type: FINAL_ANSWER`, `task_name`, `child_thread_id`, `payload`) — no second lookup is needed.
- On other exits (new user input, cancellation, timeout), any completed results are re-delivered through an `AgentResult` message.
- The default timeout is 30 seconds. You may wait again after a timeout.
- Do not use Shell, sleep, or polling as a substitute for WaitAgent.
