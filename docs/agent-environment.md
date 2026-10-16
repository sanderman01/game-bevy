# Agent environment

Facts about this machine and these repos that no tool reports. They apply to `game` and
`airshipgame`; `airshipgame` links here. The MCP tools are in [agent-tools.md](agent-tools.md).
Shell, Disk and the build times below describe the main Linux dev machine; elsewhere, check
before relying on them.

## Shell

- The shell is zsh. Quote every glob-shaped argument: `rg -n 'Foo' -g '*.rs'`,
  `grep -rn --include='*.rs' Foo .`. Unquoted, zsh fails with `no matches found`.
- No bare word starting with `=`: zsh expands `=cmd` to a path, so `echo =====` fails. Quote it.
- `rg`, and `grep` under Claude Code (a ugrep shell function), skip gitignored files, such as
  airshipgame's ignored binaries and tiles under `assets/`. Use `command grep -r` or
  `rg --no-ignore` when a search must cover them. The asset submodules are tracked and searched.
- Put `set -o pipefail;` in front of a pipe whose exit code matters, or drop the pipe:
  `cargo fmt --check | tail; echo $?` reports `tail`'s code.

## Processes

- Never `pkill -f <pattern>` or `killall`: the pattern also matches the shell running the
  command, which kills it (exit 144).
- Never kill the editor by pid: use `game_stop` (`take_over` / `--take-over` only when the user
  asks). Start and restart it with `game_start` / `game_restart` or `cargo game`.
- Another process you started: `pgrep -af '[m]y_tool'`, then `kill <pid>`. The brackets keep
  the pattern from matching itself. `pgrep -x` cannot match names over 15 characters; the kernel
  truncates them.

## Game ownership

- See the repo's `AGENTS.md` and [agent-tools.md](agent-tools.md#lifecycle).

## Disk

- `/tmp` and the session scratchpad are one 7.8 GB tmpfs shared by every session. When it fills,
  every Bash call fails.
- Raw video, traces (`--features trace` wrote 19 GB in 60 s), benchmark data, screenshots and run
  logs go to `scratch/` in the repo. Git ignores it; it is on the big disk and survives
  `cargo clean`.
- Large output never goes to `/tmp`; `df -h /tmp` shows how full it is.

## Git

- Never `git checkout <file>`, `git restore <file>`, `git stash` or `git reset --hard` while the
  tree has uncommitted work you did not create in this step. They discard all of it, not only
  your part.
- To drop your own debug code, delete it with Edit.
- To read an older commit, do not check it out: `git show <sha>:<path>`, `git diff <sha> -- <path>`.
- To build an older commit for an A/B test, first commit your own work on your branch. If
  `git status` still shows changes you did not make (staged, untracked or in an asset submodule),
  stop and ask the parent.
- A `git diff` patch is not a backup: it misses staged, untracked and binary changes.
- airshipgame's asset submodules: follow its `AGENTS.md`, section "Asset submodules".

## Editing

- Change files with Edit or Write, not ad-hoc `python3 - <<EOF ... replace` one-offs, which fail
  silently or half-apply. Scripts meant to stay are Rust or Python 3, per `AGENTS.md`.
- Run `cargo fmt --all` once before staging. The pre-commit hook runs `cargo fmt --check`,
  `cargo sort --check` and `cargo machete`.
- After `cargo fmt`, re-Read a file before editing it again.
- Never commit with `--no-verify` or git plumbing to get past a failing hook or a read-only
  `.git`; hand back to the parent instead. In a sandbox the hook fails writing
  `~/.cache/pre-commit`.

## Building and testing

- Use the `cargo dev-*` aliases. A plain `cargo test -p <crate>` resolves Bevy without the
  aliases' features (`dynamic_linking`) and rebuilds all of it.
- One cargo command per target directory at a time; a second one waits on `Blocking waiting for
  file lock`. `cargo game` and `cargo xtask` have their own target dirs in each repo.
  `cargo mcp-build` uses game's `target/ename-mcp-bin` from both repos.
- `cargo dev-test --no-fail-fast` runs every test binary; without it the first failing binary
  hides the rest.
- Leave a crate out with `cargo dev-test --exclude <crate>`; skip one known failure with
  `cargo dev-test -- --skip <test name>`.
- A test that fails before your change too is not yours to fix; report it with the results.
- Read-only reviewers do not run tests. The implementer runs `cargo dev-test --no-fail-fast` after
  its last change and puts the pass/fail counts and the failures' output in the reviewer's brief.

## Logs

- A game started by the launcher logs plain text to `game.log`; every lifecycle result names it
  under `log`.
- While the game runs, prefer `log_get_entries`.
- The engine (and most tools) print no colour codes when `NO_COLOR` is set, so there is no need
  to strip them with `sed`.

## Dependency source

- `cargo xtask ename_dep_src <crate>` prints the directory cargo compiles for each locked version:
  `big_space 0.13.0 git 5f25522 ~/.cargo/git/checkouts/big_space-.../5f25522`. Run it from the
  repo root; it works in both repos and answers for the repo you run it in.
- Never glob the registry for a version and never `find /`: the registry holds several versions of
  most crates. `big_space` is a git checkout, not the registry copy.
- Without a build (a read-only sandbox), see below; `Cargo.lock` also gives the version and the
  git rev.

## Read-only review sandbox (Codex)

- Cargo cannot take its build lock, so no builds, no `cargo xtask` and no test runs. Ask the
  parent for a test run.
- `cargo metadata` needs no lock and works. Dependency source there (`py` instead of `python3` on
  Windows; drop `--all-features` if it fails offline):
  `cargo metadata --format-version 1 --locked --offline --all-features | python3 -c 'import json,sys; [print(p["version"], p["manifest_path"]) for p in json.load(sys.stdin)["packages"] if p["name"] == "big_space"]'`
- Heredocs fail (`can't create temp file`): use `python3 -c '...'` or plain commands.
- Read pinned revisions with `git show <sha>:<path>`. Read files in ranges (`sed -n 1,200p`,
  `rg -n`): output over about 10k tokens is truncated.
- Nothing can be written, so images cannot be resized there. The parent captures them small
  (`max_width`) or downscales them into `scratch/` first and names those files in the brief.
- `EROFS` failures from a test writing temp files are the sandbox, not the code.

## Worktrees

- A new worktree builds cold (8 to 10 min on the dev machine). `CARGO_TARGET_DIR=<main>/target`
  reuses the main build, but then shares its lock.
- Codex in a linked worktree cannot write the main repo's `.git` (`index.lock: Read-only file
  system`). The parent commits.

## Subagents

- A reviewing or researching subagent writes its full report to
  `scratch/reports/<yyyy-mm-dd>-<topic>-<role>.md` and hands back at most 15 lines: the verdict,
  one line per finding by severity, and the report path.
- Claude Code tells subagents not to write report files, so the brief must ask for it. Paste:
  "Write your full report to `scratch/reports/<yyyy-mm-dd>-<topic>-<role>.md`; hand back at most
  15 lines: verdict, one line per finding by severity, the report path."
- Codex in a read-only sandbox cannot write the file. Ask for the full report as its final
  message, opening with the 15-line summary, and run
  `codex exec ... -o scratch/reports/<name>.md </dev/null >/dev/null 2>&1`. Read
  `sed -n 1,15p` of it before the rest.
