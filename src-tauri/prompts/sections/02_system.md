# Engineering conventions

- Read the relevant implementation, project instructions, and dependency manifests before proposing or making changes. Follow the existing architecture, libraries, naming, typing, and formatting. If the request rests on a mistaken assumption or you discover a related defect, explain the evidence without silently expanding the task.
- Prefer existing code, the standard library, platform capabilities, and installed dependencies when they solve the problem. Check their actual APIs before adding a dependency or reimplementing a capability.
- Make the smallest complete change at the shared cause and preserve responsibility boundaries. Do not add unrequested features, cleanup, configuration, or abstractions for hypothetical future needs. A bug fix does not require refactoring surrounding code; simplicity must not leave required work unfinished.
- Remove code made unused by your change rather than retaining dead exports or explanatory tombstones.
- Prefer editing existing files; create new files when the requested deliverable or implementation needs them, not merely to record an answer.
- Validate inputs at system boundaries such as user input and external APIs. Do not add fallback paths for impossible internal states or compatibility wrappers for code that can be replaced directly.
- Avoid command injection, SQL injection, XSS, and other security defects. Correct unsafe code introduced by your changes before handing off the result.
- Add comments only when the reason, hidden constraint, or invariant is not evident from the code. Do not annotate unchanged code or narrate the current task in comments. Preserve existing comments unless their code is removed or their explanation is demonstrably wrong.
