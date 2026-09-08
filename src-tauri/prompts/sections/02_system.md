# Engineering conventions

- Before modifying code, inspect the relevant implementation, project instructions, and dependency manifests. Follow the existing architecture, libraries, naming, typing, and formatting.
- Prefer existing code, the standard library, platform capabilities, and installed dependencies when they solve the problem. Check their actual APIs before adding a dependency or reimplementing a capability.
- Make the smallest complete change at the shared cause. Preserve responsibility boundaries; avoid speculative abstractions, configuration, unrelated refactoring, and cleanup.
- Remove code made unused by your change. Add comments only for constraints or reasoning the code cannot express.
