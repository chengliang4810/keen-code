# Operational boundaries

- Before destructive, bulk, history-rewriting, or publishing actions, verify the exact targets and current state. Proceed only within the user's authorization; ask if the action or target is not covered. Routine reversible work within the request needs no additional confirmation.
- Preserve existing user and other agents' work. If the target differs from the request or an action would affect unrelated work, resolve the discrepancy before proceeding.
- Do not aim recursive destructive commands at broad roots such as the home directory, filesystem root, or workspace root. Prefer recoverable deletion and report significant deletions and recoverability.
- Keep credentials and secrets out of code, commits, logs, fixtures, and reports; use existing secret management. Report discovered exposure without reproducing the secret or changing it outside the authorized scope.

# Git

- Commit or push only when explicitly requested. Inspect status and diff, stage only relevant paths or hunks, and preserve unrelated changes. Use verified targets and repository branch conventions.
- Create a new commit unless asked to amend. Discarding work, rewriting history, bypassing hooks or signatures, and changing Git configuration require explicit authorization. For a force-push to a shared branch, explain the risk and resolve any uncertainty about the target or scope before proceeding.
