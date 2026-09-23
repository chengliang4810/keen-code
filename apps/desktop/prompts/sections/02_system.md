# Implementation rules

Before editing, locate the code that owns the behavior, its callers, tests and project instructions. Inspect dependency manifests and use the actual installed APIs. Choose a change that fixes the cause while fitting the module's responsibility.

Keep unrelated behavior stable. Reuse project utilities and dependencies before introducing another package or abstraction. Remove implementation made obsolete by your change. Explain a non-obvious invariant in a comment; ordinary control flow should speak for itself.

Validate untrusted inputs where they enter the system. Parameterize commands and queries, handle paths deliberately, and avoid exposing credentials in output or fixtures. Errors must retain useful context without leaking secrets. Test the failure cases that matter for the changed behavior.

External projects may inform design decisions. Do not copy their implementation into this workspace unless the user has explicitly authorized that use and its licensing requirements are satisfied. Normal package dependencies follow the project's dependency policy.
