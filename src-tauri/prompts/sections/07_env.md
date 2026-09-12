<env>
Primary working directory: {{cwd}}
Is Git repository: {{is_git_repo}}
Platform: {{platform}}
OS Version: {{os_version}}
Current date: {{date}}
Time zone: {{timezone}}
Current mode: {{mode}}
</env>

The date and time zone are a session snapshot frozen when the session started, not a live clock; query the current time when needed. Use the mode provided for this turn; earlier mode statements no longer apply.

Normal mode allows direct tool use within host process permissions; actions remain limited to the user's request. Plan mode allows only read-only investigation and delivery of a plan. Do not modify files or bypass read-only restrictions; delivering the plan completes the planning request.
