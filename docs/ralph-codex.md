# Ralph + Codex CLI Workflow

This repository now includes a repo-local Ralph integration that treats Ralph as the outer loop and `codex exec` as the bottom execution runner.

It is a developer workflow for working on Roku. It is not part of Roku product runtime.

## What Was Added

- `scripts/ralph/ralph.sh`
  - outer loop
  - owns `.ralph/` state, iteration logs, and completion detection
- `scripts/ralph/run-codex.sh`
  - thin wrapper around `codex exec`
  - writes per-iteration JSONL events, stderr logs, and final-message files
- `scripts/ralph/CODEX.md`
  - Codex-specific Ralph prompt template
- `scripts/ralph/prd.json.example`
  - example PRD shape for `.ralph/prd.json`
- `just ralph`
  - convenience entrypoint

## State Layout

Runtime state lives under `.ralph/` and is gitignored:

- `.ralph/prd.json`
- `.ralph/progress.txt`
- `.ralph/archive/`
- `.ralph/runs/<timestamp>/`
- `.ralph/.last-branch`
- `.ralph/.last-run`

This keeps Ralph workflow state repo-local without mixing it into Roku runtime crates.

## Prerequisites

- `codex` installed and authenticated
- `jq`
- git repository with a clean enough working tree to let Codex do story-scoped commits

Optional environment knobs:

- `RALPH_CODEX_MODEL`
- `RALPH_CODEX_PROFILE`
- `RALPH_CODEX_SANDBOX`
- `RALPH_CODEX_APPROVAL`
- `RALPH_CODEX_ARGS`
- `RALPH_STATE_DIR`

## Quick Start

1. Create Ralph state:

```bash
mkdir -p .ralph
cp scripts/ralph/prd.json.example .ralph/prd.json
```

2. Edit `.ralph/prd.json` for your feature.

3. Run Ralph:

```bash
just ralph
just ralph 20
just ralph 10 .ralph-alt
```

or:

```bash
./scripts/ralph/ralph.sh 10
./scripts/ralph/ralph.sh --state-dir .ralph 10
./scripts/ralph/ralph.sh --tool codex 10
```

## How Codex Is Invoked

The integration uses:

```bash
codex -a never exec --sandbox workspace-write --json -o <last-message-file> -
```

with repo-root `-C <repo>`, plus optional model/profile/env overrides.

Why this shape:

- `--output-last-message` gives a stable completion artifact for Ralph to inspect
- `--json` gives a machine-friendly event log for debugging progress
- stderr is captured separately because Codex may emit startup warnings there
- each Ralph iteration is a fresh `codex exec` process, preserving the Ralph pattern

## How To View Progress

Primary progress surfaces:

- `.ralph/progress.txt`
- `.ralph/prd.json`
- `.ralph/.last-run`
- `.ralph/runs/<timestamp>/iteration-*.events.jsonl`
- `.ralph/runs/<timestamp>/iteration-*.stderr.log`

Useful commands:

```bash
cat .ralph/progress.txt
jq '.userStories[] | {id, title, passes}' .ralph/prd.json
cat .ralph/.last-run
tail -f "$(cat .ralph/.last-run)/iteration-001.stderr.log"
sed -n '1,20p' "$(cat .ralph/.last-run)/iteration-001.events.jsonl"
```

## Interrupt / Resume

Interrupt:

- `Ctrl-C` stops the outer loop

Resume:

- rerun `just ralph` or `./scripts/ralph/ralph.sh`
- Ralph re-reads `.ralph/prd.json` and `.ralph/progress.txt`
- the next fresh Codex iteration continues from repo state, git history, and Ralph state files

This is intentionally not Codex session resume. Ralph’s model is fresh-instance-per-iteration.

## Verified Facts

- Upstream Ralph already supports multiple bottom tools and currently ships `amp` and `claude` branches in `ralph.sh`.
- Upstream Ralph is fundamentally a shell outer loop plus `prd.json` and `progress.txt`, not a heavy runtime framework.
- Local `codex` supports non-interactive execution through `codex exec`.
- `codex exec --output-last-message` reliably writes the final assistant message to a file.
- `codex exec --json` produces JSONL events suitable for per-iteration logging.
- In this environment, Codex may emit plugin warmup warnings to stderr; the wrapper captures them into per-iteration log files instead of relying on a perfectly clean terminal stream.

## Local Adaptation Decisions

- This repo-local integration only supports `--tool codex`.
- Ralph state was moved to `.ralph/` instead of `scripts/ralph/` so scripts stay static and workflow state stays isolated.
- A dedicated `run-codex.sh` wrapper was added so `codex` flag handling, output capture, and logging stay in one maintainable place.
- The Codex prompt template uses `AGENTS.md` language instead of Claude-specific file conventions.

## Current Limits

- The default console experience is concise; detailed progress lives in JSONL and stderr log files.
- This does not generate PRDs for you; it expects `.ralph/prd.json` to exist.
- Only the Codex runner path was validated locally here.
- Fresh-iteration Ralph means no hidden cross-iteration in-memory state beyond git history and `.ralph/*`.

## Suggested Next Extensions

- add a small `status` helper for `.ralph/`
- add prettier live event rendering on top of the JSONL stream
- add a repo-local PRD bootstrap helper
- add story templates for common Roku work types
