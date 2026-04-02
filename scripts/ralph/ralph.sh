#!/usr/bin/env bash

set -euo pipefail

usage() {
	cat <<'EOF'
Usage:
  ./scripts/ralph/ralph.sh [--tool codex] [--state-dir <path>] [max-iterations]

Examples:
  ./scripts/ralph/ralph.sh
  ./scripts/ralph/ralph.sh 20
  ./scripts/ralph/ralph.sh --state-dir .ralph-smoke 1

Environment:
  RALPH_STATE_DIR       Default state dir (default: <repo>/.ralph)
  RALPH_CODEX_BIN       Codex executable (default: codex)
  RALPH_CODEX_MODEL     Optional model override for codex exec
  RALPH_CODEX_PROFILE   Optional Codex profile
  RALPH_CODEX_SANDBOX   Sandbox mode for codex exec (default: workspace-write)
  RALPH_CODEX_APPROVAL  Approval mode for codex exec (default: never)
  RALPH_CODEX_ARGS      Extra shell-split Codex args, e.g. '--search'
  RALPH_CODEX_TIMEOUT_SECONDS
                         Hard timeout for one Codex attempt (default: 1800)
  RALPH_CODEX_MAX_RETRIES
                         Retry count after the initial failed attempt (default: 2)
  RALPH_CODEX_RETRY_WAIT_SECONDS
                         Base wait before retrying a retryable failure (default: 10)
EOF
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

TOOL="codex"
MAX_ITERATIONS=10
STATE_DIR="${RALPH_STATE_DIR:-$ROOT_DIR/.ralph}"

while [[ $# -gt 0 ]]; do
	case "$1" in
	--tool)
		TOOL="$2"
		shift 2
		;;
	--tool=*)
		TOOL="${1#*=}"
		shift
		;;
	--state-dir)
		STATE_DIR="$2"
		shift 2
		;;
	--state-dir=*)
		STATE_DIR="${1#*=}"
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		if [[ "$1" =~ ^[0-9]+$ ]]; then
			MAX_ITERATIONS="$1"
			shift
		else
			echo "unknown argument: $1" >&2
			usage >&2
			exit 1
		fi
		;;
	esac
done

if [[ "$TOOL" != "codex" ]]; then
	echo "unsupported tool '$TOOL' in this repo-local integration; use --tool codex" >&2
	exit 1
fi

PRD_FILE="$STATE_DIR/prd.json"
PROGRESS_FILE="$STATE_DIR/progress.txt"
ARCHIVE_DIR="$STATE_DIR/archive"
RUNS_DIR="$STATE_DIR/runs"
LAST_BRANCH_FILE="$STATE_DIR/.last-branch"
LAST_RUN_FILE="$STATE_DIR/.last-run"

require_command() {
	local command_name="$1"
	if ! command -v "$command_name" >/dev/null 2>&1; then
		echo "missing required command: $command_name" >&2
		exit 127
	fi
}

ensure_prereqs() {
	require_command git
	require_command jq
	require_command "${RALPH_CODEX_BIN:-codex}"
}

ensure_state_layout() {
	mkdir -p "$STATE_DIR" "$ARCHIVE_DIR" "$RUNS_DIR"
}

init_progress_file() {
	if [[ ! -f "$PROGRESS_FILE" ]]; then
		{
			echo "# Ralph Progress Log"
			echo "Started: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
			echo
			echo "## Codebase Patterns"
			echo
			echo "---"
		} >"$PROGRESS_FILE"
	fi
}

relative_to_root() {
	local path="$1"
	if [[ "$path" == "$ROOT_DIR/"* ]]; then
		printf '%s\n' "${path#"$ROOT_DIR"/}"
	else
		printf '%s\n' "$path"
	fi
}

archive_previous_run_if_branch_changed() {
	if [[ ! -f "$PRD_FILE" || ! -f "$LAST_BRANCH_FILE" ]]; then
		return 0
	fi

	local current_branch last_branch date folder_name archive_folder
	current_branch="$(jq -r '.branchName // empty' "$PRD_FILE" 2>/dev/null || true)"
	last_branch="$(cat "$LAST_BRANCH_FILE" 2>/dev/null || true)"

	if [[ -z "$current_branch" || -z "$last_branch" || "$current_branch" == "$last_branch" ]]; then
		return 0
	fi

	date="$(date -u +%Y-%m-%d)"
	folder_name="$(printf '%s' "$last_branch" | sed 's|^ralph/||')"
	archive_folder="$ARCHIVE_DIR/$date-$folder_name"

	echo "Archiving previous Ralph run for branch: $last_branch"
	mkdir -p "$archive_folder"
	[[ -f "$PRD_FILE" ]] && cp "$PRD_FILE" "$archive_folder/"
	[[ -f "$PROGRESS_FILE" ]] && cp "$PROGRESS_FILE" "$archive_folder/"
	echo "  Archive saved to: $(relative_to_root "$archive_folder")"

	{
		echo "# Ralph Progress Log"
		echo "Started: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
		echo
		echo "## Codebase Patterns"
		echo
		echo "---"
	} >"$PROGRESS_FILE"
}

track_current_branch() {
	if [[ ! -f "$PRD_FILE" ]]; then
		return 0
	fi

	local current_branch
	current_branch="$(jq -r '.branchName // empty' "$PRD_FILE" 2>/dev/null || true)"
	if [[ -n "$current_branch" ]]; then
		printf '%s\n' "$current_branch" >"$LAST_BRANCH_FILE"
	fi
}

pending_story_count() {
	jq '[.userStories[]? | select(.passes != true)] | length' "$PRD_FILE"
}

render_prompt() {
	local prompt_path="$1"
	cat >"$prompt_path" <<EOF
# Ralph Run Context

Repository root: $ROOT_DIR
Ralph state directory: $STATE_DIR
Current iteration: $2 of $MAX_ITERATIONS

Use these files as the Ralph source of truth:
- PRD: $PRD_FILE
- Progress log: $PROGRESS_FILE

Write Ralph runtime artifacts only under:
- $(relative_to_root "$STATE_DIR")

If there is no user story with \`passes: false\` when you begin, reply exactly with \`<promise>COMPLETE</promise>\` and make no repository changes.

EOF
	cat "$SCRIPT_DIR/CODEX.md" >>"$prompt_path"
}

show_missing_prd_help() {
	cat <<EOF >&2
Missing Ralph PRD: $(relative_to_root "$PRD_FILE")

To start:
1. Create the state directory:
   mkdir -p "$(relative_to_root "$STATE_DIR")"
2. Copy the example PRD:
   cp scripts/ralph/prd.json.example "$(relative_to_root "$PRD_FILE")"
3. Edit the PRD stories for your feature.
EOF
}

main() {
	ensure_prereqs
	ensure_state_layout
	init_progress_file

	if [[ ! -f "$PRD_FILE" ]]; then
		show_missing_prd_help
		exit 1
	fi

	archive_previous_run_if_branch_changed
	track_current_branch

	local run_id run_dir
	run_id="$(date -u +%Y%m%dT%H%M%SZ)"
	run_dir="$RUNS_DIR/$run_id"
	mkdir -p "$run_dir"
	printf '%s\n' "$run_dir" >"$LAST_RUN_FILE"

	echo "Starting Ralph"
	echo "  Tool: $TOOL"
	echo "  Max iterations: $MAX_ITERATIONS"
	echo "  State dir: $(relative_to_root "$STATE_DIR")"
	echo "  Pending stories: $(pending_story_count)"
	echo "  Run dir: $(relative_to_root "$run_dir")"

	local i
	for i in $(seq 1 "$MAX_ITERATIONS"); do
		local iteration_label prompt_file last_message_file output rc
		iteration_label="$(printf 'iteration-%03d' "$i")"
		prompt_file="$run_dir/$iteration_label.prompt.md"
		last_message_file="$run_dir/$iteration_label.last-message.txt"

		render_prompt "$prompt_file" "$i"

		echo
		echo "==============================================================="
		echo "  Ralph Iteration $i of $MAX_ITERATIONS ($TOOL)"
		echo "==============================================================="
		echo "  Pending stories before iteration: $(pending_story_count)"

		if "$SCRIPT_DIR/run-codex.sh" \
			--repo-root "$ROOT_DIR" \
			--prompt-file "$prompt_file" \
			--run-dir "$run_dir" \
			--iteration "$iteration_label" >"$last_message_file.run"; then
			rc=0
		else
			rc=$?
		fi

		output="$(cat "$last_message_file.run" 2>/dev/null || true)"
		rm -f "$last_message_file.run"

		if [[ -z "$output" && -f "$last_message_file" ]]; then
			output="$(cat "$last_message_file")"
		fi

		if [[ -n "$output" ]]; then
			echo
			echo "  Final agent message:"
			printf '%s\n' "$output"
		fi

		if [[ "$output" == *"<promise>COMPLETE</promise>"* ]]; then
			echo
			echo "Ralph completed all tasks."
			echo "Completed at iteration $i of $MAX_ITERATIONS"
			echo "Progress log: $(relative_to_root "$PROGRESS_FILE")"
			echo "Run logs: $(relative_to_root "$run_dir")"
			exit 0
		fi

		if [[ "$rc" -ne 0 ]]; then
			echo "  Codex iteration exited with status $rc." >&2
			echo "  Runner status: $(relative_to_root "$run_dir/$iteration_label.status.txt")" >&2
			echo "  Stderr log: $(relative_to_root "$run_dir/$iteration_label.stderr.log")" >&2
			echo "Ralph stopped after a runner failure so the next launch can resume from persisted repo/PRD state." >&2
			exit "$rc"
		fi

		echo "Iteration $i complete. Continuing..."
		sleep 2
	done

	echo
	echo "Ralph reached max iterations ($MAX_ITERATIONS) without completing all tasks."
	echo "Progress log: $(relative_to_root "$PROGRESS_FILE")"
	echo "Run logs: $(relative_to_root "$run_dir")"
	exit 1
}

main "$@"
