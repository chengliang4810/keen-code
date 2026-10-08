# RCode

[简体中文](README.md) | [English](README.en.md)

<img src="apps/desktop/public/logo.png" width="96" height="96" alt="RCode" />

RCode is an agent development workbench organized around projects and independent tasks. R stands for Rust and Result.

The conversation is the main workspace. Files, Git, terminals, editors and previews are available in the development tools panel.

Applications live in `apps/`, shared Rust libraries in `crates/`, and shared frontend packages in `packages/`. Desktop and the standalone agent CLI have runnable entry points; TUI and Web currently have scope documentation. See the [workspace guide](docs/workspace.md).

## Features

- Project and task management with persistent conversations and drafts.
- Native Rust agents supporting Chat Completions, Responses and Messages, including streaming, reasoning, tool calls and approvals.
- File exploration, code editing, Git changes and history, terminals and web previews.
- Custom roles, read-only sub-agents, MCP servers, skills, plugins and commands.
- Workspace memory and file-based agent instructions.
- Custom model providers and local models.

## Development

Requirements: Node.js 22+, pnpm, Rust 1.95+ and the [Tauri platform prerequisites](https://tauri.app/start/prerequisites/).

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

## Checks

```sh
pnpm lint
pnpm check-types
pnpm test
pnpm build
pnpm knip
pnpm size
```

Knip, bundle size checks and Nix distribution configuration live in `config/`. See the [configuration guide](config/README.md).

Run the Rust workspace checks from the repository root:

```sh
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Build

```sh
pnpm tauri build
```

## User files

RCode stores its settings, conversations, roles, commands, skills and workspace memory under `~/.rcode`.

Agent instructions combine the current role, `~/.rcode/AGENTS.md` and the project-root `AGENTS.md`. Project skills are discovered from `.agents/skills`.

## License

RCode code is released under the MIT License. See [LICENSE](LICENSE).

RCode is based on Terax (https://github.com/crynta/terax-ai) with extensive modifications. Terax is Copyright 2026 Crynta, licensed under the Apache License 2.0; its license text is retained in [LICENSES/Terax-Apache-2.0.txt](LICENSES/Terax-Apache-2.0.txt). Third-party component notices are listed in [NOTICE](NOTICE) and [LICENSES/](LICENSES).
