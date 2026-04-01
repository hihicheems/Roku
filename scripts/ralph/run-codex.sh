#!/usr/bin/env bash

set -euo pipefail

usage() {
	cat <<'EOF'
Usage:
  ./scripts/ralph/run-codex.sh --repo-root <path> --prompt-file <path> --run-dir <path> --iteration <label>

Environment:
  RALPH_CODEX_BIN        Codex executable to run (default: codex)
  RALPH_CODEX_MODEL      Optional model override
  RALPH_CODEX_PROFILE    Optional Codex profile name
  RALPH_CODEX_SANDBOX    Codex sandbox mode (default: workspace-write)
  RALPH_CODEX_APPROVAL   Codex approval policy (default: never)
  RALPH_CODEX_ARGS       Extra shell-split args appended before the prompt

Behavior:
  - Writes JSONL events to <run-dir>/<iteration>.events.jsonl
  - Writes Codex stderr/banner output to <run-dir>/<iteration>.stderr.log
  - Writes the final assistant message to <run-dir>/<iteration>.last-message.txt
  - Prints only the final assistant message to stdout
EOF
}

REPO_ROOT=""
PROMPT_FILE=""
RUN_DIR=""
ITERATION=""

while [[ $# -gt 0 ]]; do
	case "$1" in
	--repo-root)
		REPO_ROOT="$2"
		shift 2
		;;
	--prompt-file)
		PROMPT_FILE="$2"
		shift 2
		;;
	--run-dir)
		RUN_DIR="$2"
		shift 2
		;;
	--iteration)
		ITERATION="$2"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "unknown argument: $1" >&2
		usage >&2
		exit 1
		;;
	esac
done

if [[ -z "$REPO_ROOT" || -z "$PROMPT_FILE" || -z "$RUN_DIR" || -z "$ITERATION" ]]; then
	echo "missing required arguments" >&2
	usage >&2
	exit 1
fi

CODEX_BIN="${RALPH_CODEX_BIN:-codex}"
CODEX_SANDBOX="${RALPH_CODEX_SANDBOX:-workspace-write}"
CODEX_APPROVAL="${RALPH_CODEX_APPROVAL:-never}"

if ! command -v "$CODEX_BIN" >/dev/null 2>&1; then
	echo "missing Codex CLI: $CODEX_BIN" >&2
	exit 127
fi

mkdir -p "$RUN_DIR"

EVENT_LOG="$RUN_DIR/$ITERATION.events.jsonl"
STDERR_LOG="$RUN_DIR/$ITERATION.stderr.log"
LAST_MESSAGE_FILE="$RUN_DIR/$ITERATION.last-message.txt"

cmd=("$CODEX_BIN" -a "$CODEX_APPROVAL" exec -C "$REPO_ROOT" --sandbox "$CODEX_SANDBOX" --skip-git-repo-check --color never --json -o "$LAST_MESSAGE_FILE")

if [[ -n "${RALPH_CODEX_MODEL:-}" ]]; then
	cmd+=(-m "$RALPH_CODEX_MODEL")
fi

if [[ -n "${RALPH_CODEX_PROFILE:-}" ]]; then
	cmd+=(-p "$RALPH_CODEX_PROFILE")
fi

if [[ -n "${RALPH_CODEX_ARGS:-}" ]]; then
	# Intentionally shell-split to make repo-local env overrides easy to use.
	# Example: RALPH_CODEX_ARGS='--search --enable foo'
	read -r -a extra_args <<<"$RALPH_CODEX_ARGS"
	cmd+=("${extra_args[@]}")
fi

cmd+=(-)

echo "  Codex runner: $CODEX_BIN" >&2
echo "  Codex event log: $EVENT_LOG" >&2
echo "  Codex stderr log: $STDERR_LOG" >&2
echo "  Codex last message: $LAST_MESSAGE_FILE" >&2

if ! "${cmd[@]}" <"$PROMPT_FILE" >"$EVENT_LOG" 2>"$STDERR_LOG"; then
	rc=$?
	if [[ -s "$LAST_MESSAGE_FILE" ]]; then
		cat "$LAST_MESSAGE_FILE"
	fi
	exit "$rc"
fi

if [[ -s "$LAST_MESSAGE_FILE" ]]; then
	cat "$LAST_MESSAGE_FILE"
fi
