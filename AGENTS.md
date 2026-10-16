@~/AGENTS.md

# Bevy Game — Project Guide

Supplements [~/AGENTS.md]. This file covers only what is specific to this project.
Where the two overlap, this file wins.

## Where things are written down

| Document | Covers |
| --- | --- |
| [README.md](README.md) | Overview: what this project is. |
| [docs/design.md](docs/design.md) | Design record: every decision, its reasoning, and which questions are still open. Indexes the per-topic files in `docs/design/`. Read it before doing design work. |
| [docs/conventions.md](docs/conventions.md) | Things like coordinate system axes handedness, units, socket names, exact import/export settings. |
| [docs/agent-tools.md](docs/agent-tools.md) | How to drive the running game from a coding agent: the MCP tools, what they can see, and how to run them. |
| [docs/code-style.md](docs/code-style.md) | How the Rust code is written: modern Rust, lifetime rules, crates, dependencies, and API hygiene. Read it before writing or reviewing code. |
| [docs/agent-environment.md](docs/agent-environment.md) | This machine and repo for agents: shell quoting, processes, disk, git commands that lose work, tests, logs, dependency source, review sandboxes, subagent reports. |

## What this project is

## Architecture rules

- New or changed functionality must work on Linux, macOS and Windows. Avoid Linux-only
  mechanisms (`/proc`, signals as the only stop path, bash-only scripts); prefer Rust and
  cross-platform crates, with per-OS arms behind one abstraction where needed. Existing shell
  scripts may stay as they are.
- Write new scripts in Rust or modern Python 3 rather than shell, since both run on every
  platform. In Python, use `pathlib`, `subprocess.run` and f-strings; no Python 2 idioms.

## Version Control

- At the start of a task, switch to a new branch.
- For each logical self-contained independent set of changes, do a commit after veryifying correctness and build success.
- Only commit those files that you have been working on during that task or session.
- If a task is long or complex, then split work over multiple commits.
- If you have not commited at the end of a task. Then do so before prompting the user.
- Follow the Conventional Commits specification for commit messages. Scope should be a specific topic. 
- **Keep commit messages short, one or two sentences max.**

## Building and testing

Use `cargo clippy` and `cargo build` to check for correctness and build success.
To test at runtime, start the editor with `game_start` (MCP) or `cargo game start`, and rebuild
it with `game_restart` / `cargo game restart`; never `pkill`. They run `example_game_editor_bin`,
the only target that links `ename_remote`, so the only one the MCP tools in
[docs/agent-tools.md](docs/agent-tools.md) can reach. On the CLI, pass `--owner` (from
`cargo game owner --new`). Stop or restart only games you or your parent agent started, and stop
yours before you finish.
Read [docs/agent-environment.md](docs/agent-environment.md) before running processes, tests or
git commands that discard changes.

## Code style

See [docs/code-style.md](docs/code-style.md). Read it before writing Rust code. Prefer Bevy code standard over generic Rust code standards unless enforced by cargo fmt. `cargo fmt` always wins.

## Assets and config

- **Never edit or commit binary assets, unless specifically requested.**  When a change requires one, give the user precise in-editor steps and values to set.
