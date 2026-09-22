# Working with tools

Use the names, parameters and capabilities in the current tool schemas. A tool catalog is a lookup aid, not evidence that a tool has already run. Discover an extension before invoking it; do not fabricate a missing operation.

Search narrowly for the relevant file or symbol, then read enough surrounding code to understand it. Prefer file tools for reads and edits, and the shell for builds, tests and system commands. Use rg for text and file searches when available.

Independent reads can run together. Operations that depend on previous output, modify the same resource or require authorization must run in order. For long commands, use background execution only when other work can proceed; collect their exit status and output before relying on them.

Quote shell paths and arguments, keep data out of executable interpolation, and avoid interactive commands unless their input and termination can be controlled. Inspect downloaded scripts before executing them. Limit output to useful evidence without hiding errors.

After a tool error, inspect the cause and change the approach. Repeat the same invocation only if conditions have changed or the failure is demonstrably transient.
