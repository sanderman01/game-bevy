@~/AGENTS.md

# Bevy Game — Project Guide

Supplements [~/AGENTS.md]. This file covers only what is specific to this project.
Where the two overlap, this file wins.

## Where things are written down

| Document | Covers |
| --- | --- |
| [README.md](README.md) | Overview: what this project is. |
| [docs/design.md](docs/design.md) | Design record: every decision, its reasoning, and which questions are still open. Read it before doing design work. |
| [docs/conventions.md](docs/conventions.md) | Things like coordinate system axes handedness, units, socket names, exact import/export settings. |
| [docs/code-style.md](docs/code-style.md) | How the Rust code is written: modern Rust, lifetime rules, crates, dependencies, and API hygiene. Read it before writing or reviewing code. |

## What this project is

## Architecture rules

## Version Control

- At the start of a task, switch to a new branch.
- For each logical self-contained independent set of changes, do a commit after veryifying correctness and build success.
- Only commit those files that you have been working on during that task or session.
- If a task is long or complex, then split work over multiple commits.
- If you have not commited at the end of a task. Then do so before prompting the user.
- Follow the Conventional Commits specification for commit messages. Scope should be a specific topic. 
- **Keep commit messages short, one or two sentences max.**

## Building and testing

Use `cargo check` and `cargo build` to check for correctness and build success.
Use `cargo run` to test at runtime.

## Code style

See [docs/code-style.md](docs/code-style.md). Read it before writing Rust code. Prefer Bevy code standard over generic Rust code standards unless enforced by cargo fmt. `cargo fmt` always wins.

## Assets and config

- **Never edit or commit binary assets, unless specifically requested.**  When a change requires one, give the user precise in-editor steps and values to set.
