# Ralph + Codex CLI Workflow

This repository includes a repo-local Ralph integration that treats Ralph as the outer loop and `codex exec` as the bottom execution runner.

It is a developer workflow for working on Roku. It is not part of Roku product runtime.

## What Was Added

- `scripts/ralph/ralph.sh`
  - outer loop state machine
  - owns story selection, semantic gating, commits, `passes=true`, and progress updates
- `scripts/ralph/run-codex.sh`
  - thin wrapper around `codex exec`
  - supports `--purpose execute|eval|final-eval`
  - writes per-round JSONL events, stderr logs, and final-message files
- `scripts/ralph/CODEX.md`
  - execution-only prompt contract
- `scripts/ralph/EVAL.md`
  - semantic-eval prompt contract
- `scripts/ralph/FINAL_EVAL.md`
  - run-level final eval prompt contract
- `scripts/ralph/FINAL_FIX.md`
  - bounded final-fix prompt contract
- `scripts/ralph/prd.json.example`
  - example PRD shape for `.ralph/prd.json`
- `scripts/ralph/test-semantic-eval.sh`
  - mock harness validation for the repo-local Ralph contract
- `just ralph`
  - convenience entrypoint

## State Layout

Runtime state lives under `.ralph/` and is gitignored:

- `.ralph/prd.json`
- `.ralph/prd-source.md`
- `.ralph/progress.txt`
- `.ralph/completed-stories.json`
- `.ralph/active-story.json`
- `.ralph/archive/<timestamp>-<branch>-<prd-hash>.prd.json`
- `.ralph/archive/<timestamp>-<branch>-<prd-hash>.progress.txt`
- `.ralph/archive/<timestamp>-<branch>-<prd-hash>.meta.txt`
- `.ralph/runs/<timestamp>/`
- `.ralph/.last-branch`
- `.ralph/.last-run`

This keeps Ralph workflow state repo-local without mixing it into Roku runtime crates.

## Core Contract

Story completion now flows through:

`execute -> mechanical gate -> semantic eval -> optional fix/re-eval -> commit -> passes=true -> progress`

Run completion now flows through:

`all stories passed -> final eval -> optional final fix/re-eval -> archive -> COMPLETE`

Important ownership rules:

- the execution agent does **not** commit
- the execution agent does **not** update `prd.json`
- the execution agent does **not** update `progress.txt`
- Ralph outer loop is the only authority that may:
  - create the final git commit
  - mark `passes=true`
  - append progress

Every story defaults to mandatory semantic eval before completion.
Every completed run also defaults to mandatory final eval before Ralph may declare the PRD complete.

## Prerequisites

- `codex` installed and authenticated
- `jq`
- git repository with a clean enough working tree to let Ralph manage story-scoped commits

Optional environment knobs:

- shared:
  - `RALPH_STATE_DIR`
  - `RALPH_CODEX_BIN`
- execution:
  - `RALPH_CODEX_MODEL`
  - `RALPH_CODEX_PROFILE`
  - `RALPH_CODEX_SANDBOX`
  - `RALPH_CODEX_APPROVAL`
  - `RALPH_CODEX_ARGS`
  - `RALPH_CODEX_TIMEOUT_SECONDS`
  - `RALPH_CODEX_MAX_RETRIES`
  - `RALPH_CODEX_RETRY_WAIT_SECONDS`
  - `RALPH_CODEX_TERM_GRACE_SECONDS`
- eval:
  - `RALPH_EVAL_MODEL`
  - `RALPH_EVAL_PROFILE`
  - `RALPH_EVAL_ARGS`
  - `RALPH_EVAL_SANDBOX`
  - `RALPH_EVAL_APPROVAL`
  - `RALPH_EVAL_TIMEOUT_SECONDS`
  - `RALPH_EVAL_MAX_RETRIES`
  - `RALPH_EVAL_RETRY_WAIT_SECONDS`
  - `RALPH_EVAL_TERM_GRACE_SECONDS`
  - `RALPH_EVAL_RUNNER_MAX_RETRIES`
- semantic loop:
  - `RALPH_SEMANTIC_MAX_FIX_ROUNDS`
- final eval:
  - `RALPH_FINAL_EVAL_MODEL`
  - `RALPH_FINAL_EVAL_PROFILE`
  - `RALPH_FINAL_EVAL_ARGS`
  - `RALPH_FINAL_EVAL_SANDBOX`
  - `RALPH_FINAL_EVAL_APPROVAL`
  - `RALPH_FINAL_EVAL_TIMEOUT_SECONDS`
  - `RALPH_FINAL_EVAL_MAX_RETRIES`
  - `RALPH_FINAL_EVAL_RETRY_WAIT_SECONDS`
  - `RALPH_FINAL_EVAL_TERM_GRACE_SECONDS`
  - `RALPH_FINAL_EVAL_RUNNER_MAX_RETRIES`
  - `RALPH_FINAL_FIX_MAX_ROUNDS`

## Quick Start

1. Create Ralph state:

```bash
mkdir -p .ralph
cp scripts/ralph/prd.json.example .ralph/prd.json
cp /path/to/source-prd.md .ralph/prd-source.md
```

2. Edit `.ralph/prd.json` for your feature.

3. Run Ralph:

```bash
just ralph
just ralph 20
just ralph 10 .ralph-alt
```

`10` is only the default story-iteration cap for one launch. It is not a limit on how many stories may exist in `.ralph/prd.json`.

or:

```bash
./scripts/ralph/ralph.sh 10
./scripts/ralph/ralph.sh --state-dir .ralph 10
./scripts/ralph/ralph.sh --tool codex 10
```

## How Codex Is Invoked

The integration uses:

```bash
codex -a never exec --json -o <last-message-file> -
```

with repo-root `-C <repo>`, plus purpose-specific sandbox/configuration:

- execution defaults to `workspace-write`
- eval defaults to `read-only`

Why this shape:

- `--output-last-message` gives a stable machine-readable artifact source
- `--json` gives a machine-friendly event log for debugging progress
- stderr is captured separately because Codex may emit startup warnings there
- each execution or eval round is a fresh `codex exec` process, preserving the Ralph pattern

## Execution and Eval Artifacts

Each execution round must yield a canonical artifact:

- `.ralph/runs/<run>/iteration-XXX.exec.story-result.json`

Top-level execution status:

- `ok`
- `mechanical_failed`
- `infra_fail`

Each semantic eval round must yield a canonical artifact:

- `.ralph/runs/<run>/iteration-XXX.eval.semantic-eval.json`

Top-level eval status:

- `pass`
- `soft_fail`
- `hard_fail`
- `infra_fail`

Eval output is consumed as JSON and then persisted by Ralph. This keeps evaluator runs compatible with the default read-only sandbox.

Each final eval round must yield a canonical artifact:

- `.ralph/runs/<run>/final.eval.semantic-eval.json`

If final eval enters corrective rounds, Ralph also writes:

- `.ralph/runs/<run>/final.fix-XX.exec.fix-result.json`
- `.ralph/runs/<run>/final.fix-XX.eval.semantic-eval.json`

## Semantic Gating Rules

- `execution.status=ok` is required before semantic eval runs
- `execution.status=mechanical_failed` stops the story before eval
- `execution.status=infra_fail` stops the story for manual inspection
- `semantic-eval.status=pass` is required before commit / `passes=true`
- `semantic-eval.status=soft_fail` enters a fix round
- `semantic-eval.status=hard_fail` stops the run for human intervention
- `semantic-eval.status=infra_fail` retries the evaluator first, then stops without blaming the story
- `final-eval.status=pass` is required before Ralph may archive and announce completion
- if final eval passes but the worktree is still dirty outside a final corrective round, Ralph stops and asks you either to clean the worktree or explicitly adopt that diff into `FINAL`
- `final-eval.status=soft_fail` enters a bounded final-fix round
- `final-eval.status=hard_fail` stops the run for human intervention
- `final-eval.status=infra_fail` retries the final evaluator first, then stops without blaming the implementation

`MAX_ITERATIONS` counts handled stories only. Eval retries and fix subrounds do not consume the story-iteration budget.

## Runner Hardening

The repo-local Codex runner still applies:

- hard timeout per Codex attempt
- retry on timeout / likely transport failures
- fail-fast after retries are exhausted

Execution defaults:

```bash
RALPH_CODEX_TIMEOUT_SECONDS=1800
RALPH_CODEX_MAX_RETRIES=5
RALPH_CODEX_RETRY_WAIT_SECONDS=10
RALPH_CODEX_TERM_GRACE_SECONDS=5
```

Eval defaults:

```bash
RALPH_EVAL_TIMEOUT_SECONDS=900
RALPH_EVAL_MAX_RETRIES=2
RALPH_EVAL_RETRY_WAIT_SECONDS=10
RALPH_EVAL_TERM_GRACE_SECONDS=5
RALPH_EVAL_RUNNER_MAX_RETRIES=0
RALPH_SEMANTIC_MAX_FIX_ROUNDS=3
```

Final eval defaults:

```bash
RALPH_FINAL_EVAL_TIMEOUT_SECONDS=1200
RALPH_FINAL_EVAL_MAX_RETRIES=2
RALPH_FINAL_EVAL_RETRY_WAIT_SECONDS=10
RALPH_FINAL_EVAL_RUNNER_MAX_RETRIES=0
RALPH_FINAL_FIX_MAX_ROUNDS=3
```

Retry behavior remains conservative:

- retryable at runner level: timeout, stream disconnects, TLS handshake EOF, request transport errors, common transient 5xx/network failures
- non-retryable at runner level: ordinary task/code failures

If the runner still fails after retries, Ralph preserves the checkpoint and stops.

## Dirty Worktree Adoption

If Ralph starts with a dirty worktree and no active checkpoint, it stops by default.

Explicit rescue path:

```bash
./scripts/ralph/ralph.sh --adopt-dirty-worktree US-003 --yes
```

Rules:

- only a pending story may be adopted
- without `--yes`, adoption requires interactive confirmation
- in non-interactive mode, `--yes` is mandatory
- adoption creates `.ralph/active-story.json` and continues the same story only

## Commit and State Hygiene

- commit messages still follow `.codex/rules/git-commit.md`
- commit titles should describe the actual code change only; do not include story IDs or PRD labels
- `.ralph/*` is runtime state, not product code
- when a PRD is complete, Ralph archives the finished `prd.json` and `progress.txt` under `.ralph/archive/` before the next PRD replaces the active file

## How To View Progress

Primary progress surfaces:

- `.ralph/progress.txt`
- `.ralph/prd.json`
- `.ralph/active-story.json`
- `.ralph/prd-source.md`
- `.ralph/completed-stories.json`
- `.ralph/.last-run`
- `.ralph/runs/<timestamp>/iteration-*.events.jsonl`
- `.ralph/runs/<timestamp>/iteration-*.stderr.log`
- `.ralph/runs/<timestamp>/iteration-*.status.txt`
- `.ralph/runs/<timestamp>/iteration-*.story-result.json`
- `.ralph/runs/<timestamp>/iteration-*.semantic-eval.json`
- `.ralph/runs/<timestamp>/final.eval.semantic-eval.json`
- `.ralph/runs/<timestamp>/final.fix-*.fix-result.json`

At the end of a launch, Ralph also prints a human-readable elapsed-time summary. This appears both on successful completion and when the launch stops because it hit the current story cap.

Useful commands:

```bash
cat .ralph/progress.txt
jq '.userStories[] | {id, title, passes}' .ralph/prd.json
cat .ralph/active-story.json
cat .ralph/prd-source.md
cat .ralph/completed-stories.json
cat .ralph/.last-run
cat "$(cat .ralph/.last-run)/iteration-001.exec.status.txt"
tail -f "$(cat .ralph/.last-run)/iteration-001.exec.stderr.log"
sed -n '1,20p' "$(cat .ralph/.last-run)/iteration-001.exec.events.jsonl"
```

## Interrupt / Resume

Interrupt:

- `Ctrl-C` stops the outer loop

Resume:

- rerun `just ralph` or `./scripts/ralph/ralph.sh`
- Ralph re-reads `.ralph/prd.json`, `.ralph/progress.txt`, and `.ralph/active-story.json`
- if an active story checkpoint exists, Ralph resumes the same story before touching any later story
- if the active PRD is already complete, rerunning Ralph archives the completed state and exits without launching Codex again
- if a launch stops because it hit `MAX_ITERATIONS`, rerun Ralph to continue the remaining stories in the same `prd.json`

This is intentionally not Codex session resume. Ralph’s model is fresh-instance-per-round with checkpoint-based recovery.

## Verified Facts

- upstream Ralph is fundamentally a shell outer loop plus `prd.json` and `progress.txt`, not a heavy runtime framework
- local `codex` supports non-interactive execution through `codex exec`
- `codex exec --output-last-message` reliably writes the final assistant message to a file
- `codex exec --json` produces JSONL events suitable for per-round logging
- transport failures can leave `codex exec` retrying long enough to stall the outer loop if the wrapper does not impose its own timeout/fail-fast policy

## Local Adaptation Decisions

- this repo-local integration only supports `--tool codex`
- Ralph state lives under `.ralph/`, not under `scripts/ralph/`
- a dedicated `run-codex.sh` wrapper keeps purpose-specific Codex config, output capture, and logging in one place
- evaluator verdicts are persisted by the outer loop from the final JSON message, instead of asking a read-only evaluator to write files directly

## Current Limits

- this does not generate PRDs for you; it expects `.ralph/prd.json` to exist
- only the Codex runner path is supported
- semantic eval is story-aware, but it is still prompt-driven rather than a domain-specific checker
- crash recovery is checkpoint-based, not in-process continuation
- `MAX_ITERATIONS` limits one launch only; it does not cap how many stories may be present in `prd.json`

## Validation

Repo-local harness validation:

```bash
./scripts/ralph/test-semantic-eval.sh
```

This mock test covers:

- pass -> commit -> `passes=true`
- soft-fail -> fix -> re-eval -> pass
- eval infra-fail -> retry -> pass
- dirty-worktree adoption
- mechanical failure stops before semantic eval

## Suggested Next Extensions

- add a small `status` helper for `.ralph/`
- add prettier live event rendering on top of the JSONL stream
- add a repo-local PRD bootstrap helper
- add story templates for common Roku work types
