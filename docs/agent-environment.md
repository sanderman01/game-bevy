# Agent environment

Facts about this machine and these repos that no tool reports. They apply to `game` and
`airshipgame`; `airshipgame` links here. The MCP tools are in [agent-tools.md](agent-tools.md).

## Shell

- The shell is zsh. Quote every glob-shaped argument: `rg -n 'Foo' -g '*.rs'`,
  `grep -rn --include='*.rs' Foo .`. Unquoted, zsh fails with `no matches found`.
- No bare word starting with `=`: zsh expands `=cmd` to a path, so `echo =====` fails. Quote it.
- `rg` and the shell's `grep` (a ugrep function under Claude Code) skip gitignored files, such as
  airshipgame's ignored binaries and tiles under `assets/`. Use `command grep -r` or
  `rg --no-ignore` when a search must cover them. The asset submodules are tracked and searched.
- Put `set -o pipefail;` in front of a pipe whose exit code matters, or drop the pipe:
  `cargo fmt --check | tail; echo $?` reports `tail`'s code.

## Processes

- Never `pkill -f <pattern>` or `killall`: the pattern also matches the shell running the
  command, which kills it (exit 144).
- Start, stop and restart the editor with `game_start` / `game_stop` / `game_restart`, or
  `cargo game` from a shell ([agent-tools.md](agent-tools.md#lifecycle)).
- To find another process: `pgrep -af '[e]xample_game_editor_bin'`, then `kill <pid>`. The
  brackets keep the pattern from matching itself. `pgrep -x` cannot match names over 15
  characters; the kernel truncates them.

## Game ownership

- Stop or restart only games you or your parent agent started. Anything else only when the user
  asks (`take_over`).
- When your tasks are done, stop the games you started, unless you leave one running for the user
  to inspect; then say so.
- Over MCP, ownership is automatic. On the CLI, pass `--owner <token>` or set `ENAME_GAME_OWNER`
  on every `cargo game` call. A top-level CLI agent gets a token from `cargo game owner --new`. A
  parent handing game work to a separate-process agent puts its own token (`cargo game owner`,
  or `caller` in `game_status`) in the brief as `ENAME_GAME_OWNER`.
- Never run `cargo game` without a token: no token means the user's game.

## Disk

- `/tmp` and the session scratchpad are one 7.8 GB tmpfs shared by every session. When it fills,
  every Bash call fails.
- Raw video, traces (`--features trace` wrote 19 GB in 60 s), benchmark data, screenshots and run
  logs go to `scratch/` in the repo. Git ignores it; it is on the big disk and survives
  `cargo clean`.
- Check `df -h /tmp` before writing anything over 100 MB there.

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
- Run one cargo command at a time per repo; a second one waits on `Blocking waiting for file lock`.
- `cargo dev-test --no-fail-fast` runs every test binary; without it the first failing binary
  hides the rest.
- Leave a crate out with `cargo dev-test --exclude <crate>`; skip one known failure with
  `cargo dev-test -- --skip <test name>`.
- A test that fails before your change too is not yours to fix; report it with the results.

## Logs

- A game started by the launcher logs plain text to `game.log`; every lifecycle result names it
  under `log`.
- While the game runs, prefer `log_get_entries`.
- For other output, `NO_COLOR=1` turns off the engine's terminal colours, so no `sed` to strip
  escape codes: `NO_COLOR=1 cargo dev-run > scratch/run.log 2>&1` (humans' route; agents use the
  launcher).

## Dependency source

- `cargo xtask ename_dep_src <crate>` prints the directory cargo compiles for each locked version:
  `big_space 0.13.0 git 5f25522 /home/sander/.cargo/git/checkouts/...`. It works in both repos and
  answers for the repo you run it in.
- Never glob the registry for a version and never `find /`. Bevy is 0.19.1; the registry also
  holds 0.20.0 and older. `big_space` is a git checkout, not the registry copy.
- Without a build (a read-only sandbox), see below; `Cargo.lock` also gives the version and the
  git rev.

## Read-only review sandbox (Codex)

- Cargo cannot take its build lock, so no builds, no `cargo xtask` and no test runs. The brief
  carries the implementer's `cargo dev-test --no-fail-fast` result; ask the parent for another run.
- `cargo metadata` needs no lock and works. Dependency source there:
  `cargo metadata --format-version 1 --locked --offline | python3 -c 'import json,sys; [print(p["version"], p["manifest_path"]) for p in json.load(sys.stdin)["packages"] if p["name"] == "big_space"]'`
- Heredocs fail (`can't create temp file`): use `python3 -c '...'` or plain commands.
- Read pinned revisions with `git show <sha>:<path>`. Read files in ranges (`sed -n 1,200p`,
  `rg -n`): output over about 10k tokens is truncated.
- Nothing can be written, so images cannot be resized there. The parent captures them small
  (`max_width`) or downscales them into `scratch/` first and names those files in the brief.
- `EROFS` failures from a test writing temp files are the sandbox, not the code.

## Worktrees

- A new worktree builds cold (8 to 10 min). `CARGO_TARGET_DIR=<main>/target` reuses the main
  build, but then shares its lock.
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
