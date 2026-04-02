#!/usr/bin/env bash

set -euo pipefail

usage() {
	cat <<'EOF'
Usage:
  ./scripts/ralph/ralph.sh [--tool codex] [--state-dir <path>] [--adopt-dirty-worktree <story-id>] [--yes] [max-iterations]

Examples:
  ./scripts/ralph/ralph.sh
  ./scripts/ralph/ralph.sh 20
  ./scripts/ralph/ralph.sh --state-dir .ralph-smoke 1
  ./scripts/ralph/ralph.sh --adopt-dirty-worktree US-003 --yes

Environment:
  Shared:
    RALPH_STATE_DIR                Default state dir (default: <repo>/.ralph)
    RALPH_CODEX_BIN                Codex executable (default: codex)

  Execution:
    RALPH_CODEX_MODEL             Optional model override for codex exec
    RALPH_CODEX_PROFILE           Optional Codex profile
    RALPH_CODEX_SANDBOX           Sandbox mode for codex exec (default: workspace-write)
    RALPH_CODEX_APPROVAL          Approval mode for codex exec (default: never)
    RALPH_CODEX_ARGS              Extra shell-split Codex args, e.g. '--search'
    RALPH_CODEX_TIMEOUT_SECONDS   Hard timeout for one Codex attempt (default: 1800)
    RALPH_CODEX_MAX_RETRIES       Retry count after the initial failed attempt (default: 5)
    RALPH_CODEX_RETRY_WAIT_SECONDS
                                  Base wait before retrying a retryable failure (default: 10)
    RALPH_CODEX_TERM_GRACE_SECONDS
                                  Grace period between TERM and KILL on timeout (default: 5)

  Eval:
    RALPH_EVAL_MODEL              Optional evaluator model override
    RALPH_EVAL_PROFILE            Optional evaluator Codex profile
    RALPH_EVAL_ARGS               Extra shell-split evaluator args
    RALPH_EVAL_SANDBOX            Evaluator sandbox mode (default: read-only)
    RALPH_EVAL_APPROVAL           Evaluator approval mode (default: never)
    RALPH_EVAL_TIMEOUT_SECONDS    Hard timeout for one evaluator attempt (default: 900)
    RALPH_EVAL_MAX_RETRIES        Outer-loop evaluator infra retries (default: 2)
    RALPH_EVAL_RETRY_WAIT_SECONDS Base wait before retrying evaluator infra failures (default: 10)
    RALPH_EVAL_TERM_GRACE_SECONDS Grace period between TERM and KILL on evaluator timeout (default: 5)
    RALPH_EVAL_RUNNER_MAX_RETRIES Runner retry count for evaluator transport failures (default: 0)

  Semantic loop:
    RALPH_SEMANTIC_MAX_FIX_ROUNDS Fix rounds after evaluator soft-fail (default: 2)

  Note:
    max-iterations limits one Ralph launch only; `.ralph/prd.json` may contain more stories
    than this number. Eval and fix subrounds do not consume the story iteration budget.
EOF
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

TOOL="codex"
MAX_ITERATIONS=10
STATE_DIR="${RALPH_STATE_DIR:-$ROOT_DIR/.ralph}"
ADOPT_DIRTY_STORY_ID=""
ASSUME_YES=0
RALPH_EVAL_MAX_RETRIES="${RALPH_EVAL_MAX_RETRIES:-2}"
RALPH_EVAL_RETRY_WAIT_SECONDS="${RALPH_EVAL_RETRY_WAIT_SECONDS:-10}"
RALPH_SEMANTIC_MAX_FIX_ROUNDS="${RALPH_SEMANTIC_MAX_FIX_ROUNDS:-2}"

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
	--adopt-dirty-worktree)
		ADOPT_DIRTY_STORY_ID="$2"
		shift 2
		;;
	--adopt-dirty-worktree=*)
		ADOPT_DIRTY_STORY_ID="${1#*=}"
		shift
		;;
	--yes)
		ASSUME_YES=1
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
LAST_ARCHIVE_KEY_FILE="$STATE_DIR/.last-archive-key"
ACTIVE_STORY_FILE="$STATE_DIR/active-story.json"

require_command() {
	local command_name="$1"
	if ! command -v "$command_name" >/dev/null 2>&1; then
		echo "missing required command: $command_name" >&2
		exit 127
	fi
}

ensure_uint() {
	local label="$1"
	local value="$2"
	if [[ ! "$value" =~ ^[0-9]+$ ]]; then
		echo "$label must be an unsigned integer, got: $value" >&2
		exit 2
	fi
}

ensure_prereqs() {
	require_command git
	require_command jq
	require_command shasum
	require_command "${RALPH_CODEX_BIN:-codex}"
	ensure_uint "RALPH_EVAL_MAX_RETRIES" "$RALPH_EVAL_MAX_RETRIES"
	ensure_uint "RALPH_EVAL_RETRY_WAIT_SECONDS" "$RALPH_EVAL_RETRY_WAIT_SECONDS"
	ensure_uint "RALPH_SEMANTIC_MAX_FIX_ROUNDS" "$RALPH_SEMANTIC_MAX_FIX_ROUNDS"
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

timestamp_utc() {
	date -u +"%Y-%m-%dT%H:%M:%SZ"
}

relative_to_root() {
	local path="$1"
	if [[ "$path" == "$ROOT_DIR/"* ]]; then
		printf '%s\n' "${path#"$ROOT_DIR"/}"
	else
		printf '%s\n' "$path"
	fi
}

current_branch_name() {
	local git_branch prd_branch
	git_branch="$(git -C "$ROOT_DIR" branch --show-current 2>/dev/null || true)"
	if [[ -n "$git_branch" && "$git_branch" != "HEAD" ]]; then
		printf '%s\n' "$git_branch"
		return 0
	fi

	prd_branch="$(jq -r '.branchName // empty' "$PRD_FILE" 2>/dev/null || true)"
	if [[ -n "$prd_branch" ]]; then
		printf '%s\n' "$prd_branch"
		return 0
	fi

	printf 'unlabeled\n'
}

sanitize_archive_label() {
	printf '%s' "$1" | sed 's|[^A-Za-z0-9._-]|-|g; s|-\\{2,\\}|-|g; s|^-||; s|-$||'
}

archive_current_state() {
	local reason="$1"
	if [[ ! -f "$PRD_FILE" ]]; then
		return 0
	fi

	local raw_branch branch_label prd_hash archive_key timestamp archive_base
	raw_branch="$(current_branch_name)"
	branch_label="$(sanitize_archive_label "$raw_branch")"
	if [[ -z "$branch_label" ]]; then
		branch_label="unlabeled"
	fi

	prd_hash="$(shasum -a 256 "$PRD_FILE" | awk '{print substr($1, 1, 12)}')"
	archive_key="${branch_label}-${prd_hash}"

	if [[ -f "$LAST_ARCHIVE_KEY_FILE" ]] && [[ "$(cat "$LAST_ARCHIVE_KEY_FILE")" == "$archive_key" ]]; then
		return 0
	fi

	if compgen -G "$ARCHIVE_DIR/*-${archive_key}.prd.json" >/dev/null; then
		printf '%s\n' "$archive_key" >"$LAST_ARCHIVE_KEY_FILE"
		return 0
	fi

	timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
	archive_base="$ARCHIVE_DIR/${timestamp}-${archive_key}"

	cp "$PRD_FILE" "${archive_base}.prd.json"
	if [[ -f "$PROGRESS_FILE" ]]; then
		cp "$PROGRESS_FILE" "${archive_base}.progress.txt"
	fi

	cat >"${archive_base}.meta.txt" <<EOF
archived_at=$(timestamp_utc)
reason=$reason
source_branch=$raw_branch
prd_hash=$prd_hash
EOF

	printf '%s\n' "$archive_key" >"$LAST_ARCHIVE_KEY_FILE"
	echo "Archived Ralph state: $(relative_to_root "${archive_base}.prd.json")"
}

track_current_branch() {
	printf '%s\n' "$(current_branch_name)" >"$LAST_BRANCH_FILE"
}

pending_story_count() {
	jq '[.userStories[]? | select(.passes != true)] | length' "$PRD_FILE"
}

total_story_count() {
	jq '[.userStories[]?] | length' "$PRD_FILE"
}

story_exists_pending() {
	local story_id="$1"
	jq -e --arg story_id "$story_id" '.userStories[]? | select(.id == $story_id and .passes != true)' "$PRD_FILE" >/dev/null
}

next_pending_story_id() {
	jq -r '
		.userStories
		| map(select(.passes != true))
		| sort_by(.priority, .id)
		| .[0].id // empty
	' "$PRD_FILE"
}

selected_story_payload() {
	local story_id="$1"
	jq -c --arg story_id "$story_id" '
		.userStories[]
		| select(.id == $story_id)
		| {
			id,
			title,
			description,
			priority,
			acceptanceCriteria: (
				.acceptanceCriteria
				| to_entries
				| map({
					criterionId: ("AC-" + ((.key + 1) | tostring)),
					criterionText: .value
				})
			)
		}
	' "$PRD_FILE"
}

story_title() {
	local story_id="$1"
	jq -r --arg story_id "$story_id" '.userStories[] | select(.id == $story_id) | .title' "$PRD_FILE"
}

dirty_worktree_files_json() {
	local lines
	lines="$(
		{
			git -C "$ROOT_DIR" diff --name-only --relative
			git -C "$ROOT_DIR" diff --cached --name-only --relative
			git -C "$ROOT_DIR" ls-files --others --exclude-standard
		} | sed '/^[[:space:]]*$/d' | grep -v '^.ralph/' | sort -u
	)"

	if [[ -z "$lines" ]]; then
		printf '[]\n'
	else
		printf '%s\n' "$lines" | jq -R . | jq -s '.'
	fi
}

dirty_worktree_count() {
	local dirty_json
	dirty_json="$(dirty_worktree_files_json)"
	jq 'length' <<<"$dirty_json"
}

show_dirty_worktree_summary() {
	local dirty_json
	dirty_json="$(dirty_worktree_files_json)"
	if [[ "$(jq 'length' <<<"$dirty_json")" -eq 0 ]]; then
		echo "  Dirty files: none"
		return
	fi

	echo "  Dirty files:"
	jq -r '.[]' <<<"$dirty_json" | sed 's/^/    - /'
}

write_active_story_checkpoint() {
	local story_id="$1"
	local story_iteration="$2"
	local phase="$3"
	local fix_round="$4"
	local run_dir="$5"
	local execution_artifact_path="$6"
	local eval_artifact_path="$7"
	local dirty_json
	dirty_json="$(dirty_worktree_files_json)"

	jq -n \
		--arg storyId "$story_id" \
		--argjson storyIteration "$story_iteration" \
		--arg phase "$phase" \
		--argjson fixRound "$fix_round" \
		--arg runDir "$run_dir" \
		--arg executionArtifactPath "$execution_artifact_path" \
		--arg evalArtifactPath "$eval_artifact_path" \
		--arg startedAt "$(timestamp_utc)" \
		--argjson worktreeDirtyFiles "$dirty_json" \
		'{
			storyId: $storyId,
			storyIteration: $storyIteration,
			phase: $phase,
			fixRound: $fixRound,
			runDir: $runDir,
			executionArtifactPath: $executionArtifactPath,
			evalArtifactPath: $evalArtifactPath,
			startedAt: $startedAt,
			worktreeDirtyFiles: $worktreeDirtyFiles
		}' >"$ACTIVE_STORY_FILE"
}

clear_active_story_checkpoint() {
	rm -f "$ACTIVE_STORY_FILE"
}

adopt_dirty_worktree_confirmed() {
	if [[ "$ASSUME_YES" -eq 1 ]]; then
		return 0
	fi

	if [[ ! -t 0 ]]; then
		echo "Dirty worktree adoption requires --yes in non-interactive mode." >&2
		return 1
	fi

	local answer
	printf "Adopt dirty worktree into story %s? [y/N] " "$ADOPT_DIRTY_STORY_ID" >&2
	read -r answer
	case "$answer" in
	y | Y | yes | YES)
		return 0
		;;
	*)
		echo "Aborted dirty worktree adoption." >&2
		return 1
		;;
	esac
}

maybe_adopt_dirty_worktree() {
	if [[ -z "$ADOPT_DIRTY_STORY_ID" ]]; then
		return 1
	fi

	if [[ -f "$ACTIVE_STORY_FILE" ]]; then
		echo "Cannot adopt a dirty worktree while an active story checkpoint already exists." >&2
		exit 1
	fi

	if ! story_exists_pending "$ADOPT_DIRTY_STORY_ID"; then
		echo "Cannot adopt dirty worktree into unknown or completed story: $ADOPT_DIRTY_STORY_ID" >&2
		exit 1
	fi

	if [[ "$(dirty_worktree_count)" -eq 0 ]]; then
		echo "Nothing to adopt: the worktree is clean." >&2
		exit 1
	fi

	echo "Preparing dirty worktree adoption."
	echo "  Target story: $ADOPT_DIRTY_STORY_ID - $(story_title "$ADOPT_DIRTY_STORY_ID")"
	show_dirty_worktree_summary

	if ! adopt_dirty_worktree_confirmed; then
		exit 1
	fi

	write_active_story_checkpoint "$ADOPT_DIRTY_STORY_ID" 0 "execute" 0 "" "" ""
	echo "Adopted dirty worktree into story $ADOPT_DIRTY_STORY_ID."
	return 0
}

stop_for_dirty_worktree() {
	echo "Ralph found a dirty worktree without an active checkpoint." >&2
	show_dirty_worktree_summary >&2 || true
	echo "Either clean the worktree first or adopt it explicitly:" >&2
	echo "  ./scripts/ralph/ralph.sh --adopt-dirty-worktree <story-id> --yes" >&2
	exit 1
}

ensure_story_context() {
	if [[ ! -f "$PRD_FILE" ]]; then
		show_missing_prd_help
		exit 1
	fi

	if [[ "$(dirty_worktree_count)" -gt 0 ]]; then
		if [[ -f "$ACTIVE_STORY_FILE" ]]; then
			return 0
		fi
		if [[ -n "$ADOPT_DIRTY_STORY_ID" ]]; then
			maybe_adopt_dirty_worktree
			return 0
		fi
		stop_for_dirty_worktree
	fi
}

render_execution_prompt() {
	local prompt_path="$1"
	local story_json="$2"
	local story_iteration="$3"
	local fix_round="$4"
	local artifact_path="$5"
	local prior_eval_path="$6"

	cat >"$prompt_path" <<EOF
# Ralph Execution Context

Repository root: $ROOT_DIR
Ralph state directory: $STATE_DIR
Current story iteration: $story_iteration of $MAX_ITERATIONS
Current fix round: $fix_round

Selected story:
$story_json

Use these files as the Ralph source of truth:
- PRD: $PRD_FILE
- Progress log: $PROGRESS_FILE

Write the execution artifact JSON to:
- $artifact_path

If this is a fix round, the previous semantic eval artifact is:
- $prior_eval_path

Do not modify:
- $PRD_FILE
- $PROGRESS_FILE

EOF
	cat "$SCRIPT_DIR/CODEX.md" >>"$prompt_path"
}

render_eval_prompt() {
	local prompt_path="$1"
	local story_json="$2"
	local story_iteration="$3"
	local fix_round="$4"
	local execution_artifact_path="$5"

	cat >"$prompt_path" <<EOF
# Ralph Semantic Eval Context

Repository root: $ROOT_DIR
Ralph state directory: $STATE_DIR
Current story iteration: $story_iteration of $MAX_ITERATIONS
Current fix round: $fix_round

Selected story:
$story_json

Use these files as the Ralph source of truth:
- PRD: $PRD_FILE
- Progress log: $PROGRESS_FILE
- Execution artifact: $execution_artifact_path

Review the current uncommitted worktree against the selected story.

EOF
	cat "$SCRIPT_DIR/EVAL.md" >>"$prompt_path"
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

validate_execution_core() {
	local artifact_path="$1"
	jq -e '
		((.status == "ok") or (.status == "mechanical_failed") or (.status == "infra_fail"))
		and (.summary | type == "string")
		and (.filesChanged | type == "array")
		and all(.filesChanged[]?; type == "string")
		and (.mechanicalChecks | type == "array")
		and all(
			.mechanicalChecks[]?;
			(.command | type == "string")
			and ((.status == "passed") or (.status == "failed") or (.status == "skipped"))
			and ((has("outputPath") | not) or (.outputPath | type == "string"))
		)
		and (.acceptanceCriteriaClaims | type == "array")
		and all(
			.acceptanceCriteriaClaims[]?;
			(.criterionId | type == "string")
			and (.criterionText | type == "string")
			and ((.claimedStatus == "met") or (.claimedStatus == "not_met") or (.claimedStatus == "unclear"))
			and has("evidence")
		)
		and (.proposedCommit | type == "object")
		and (.proposedCommit.title | type == "string")
		and (.proposedCommit.bodyBullets | type == "array")
		and all(.proposedCommit.bodyBullets[]?; type == "string")
		and (.learnings | type == "array")
		and all(.learnings[]?; type == "string")
	' "$artifact_path" >/dev/null
}

validate_eval_core() {
	local artifact_path="$1"
	jq -e '
		((.status == "pass") or (.status == "soft_fail") or (.status == "hard_fail") or (.status == "infra_fail"))
		and (.summary | type == "string")
		and (.acceptanceCriteriaReview | type == "array")
		and all(
			.acceptanceCriteriaReview[]?;
			(.criterionId | type == "string")
			and (.criterionText | type == "string")
			and ((.judgment == "met") or (.judgment == "unmet") or (.judgment == "unclear"))
			and has("evidence")
		)
		and (.findings | type == "array")
		and all(.findings[]?; type == "string")
		and (.requiredFixes | type == "array")
		and all(.requiredFixes[]?; type == "string")
		and (.verdictSummary | type == "object")
		and (.verdictSummary.decision | type == "string")
		and (.verdictSummary.primaryReason | type == "string")
		and (.verdictSummary.requiredFixesCount | type == "number")
		and (
			(has("approvedCommit") | not)
			or (
				(.approvedCommit | type == "object")
				and (.approvedCommit.title | type == "string")
				and (.approvedCommit.bodyBullets | type == "array")
				and all(.approvedCommit.bodyBullets[]?; type == "string")
			)
		)
	' "$artifact_path" >/dev/null
}

normalize_execution_artifact() {
	local raw_path="$1"
	local artifact_path="$2"
	local story_id="$3"
	local story_iteration="$4"
	local fix_round="$5"
	local dirty_json git_head
	dirty_json="$(dirty_worktree_files_json)"
	git_head="$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || true)"
	if [[ -z "$git_head" ]]; then
		git_head="unborn"
	fi

	jq \
		--arg storyId "$story_id" \
		--argjson storyIteration "$story_iteration" \
		--argjson fixRound "$fix_round" \
		--arg generatedAt "$(timestamp_utc)" \
		--arg gitHead "$git_head" \
		--argjson worktreeDirtyFiles "$dirty_json" \
		'. + {
			storyId: $storyId,
			storyIteration: $storyIteration,
			fixRound: $fixRound,
			generatedAt: $generatedAt,
			gitHead: $gitHead,
			worktreeDirtyFiles: $worktreeDirtyFiles
		}' "$raw_path" >"$artifact_path"
}

write_synthetic_execution_infra_artifact() {
	local artifact_path="$1"
	local story_id="$2"
	local story_iteration="$3"
	local fix_round="$4"
	local reason="$5"
	local dirty_json git_head
	dirty_json="$(dirty_worktree_files_json)"
	git_head="$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || true)"
	if [[ -z "$git_head" ]]; then
		git_head="unborn"
	fi

	jq -n \
		--arg storyId "$story_id" \
		--argjson storyIteration "$story_iteration" \
		--argjson fixRound "$fix_round" \
		--arg generatedAt "$(timestamp_utc)" \
		--arg gitHead "$git_head" \
		--argjson worktreeDirtyFiles "$dirty_json" \
		--arg summary "$reason" \
		'{
			storyId: $storyId,
			storyIteration: $storyIteration,
			fixRound: $fixRound,
			generatedAt: $generatedAt,
			gitHead: $gitHead,
			worktreeDirtyFiles: $worktreeDirtyFiles,
			status: "infra_fail",
			summary: $summary,
			filesChanged: [],
			mechanicalChecks: [],
			acceptanceCriteriaClaims: [],
			proposedCommit: {
				title: "",
				bodyBullets: []
			},
			learnings: []
		}' >"$artifact_path"
}

normalize_eval_artifact() {
	local raw_message_path="$1"
	local artifact_path="$2"
	local story_id="$3"
	local story_iteration="$4"
	local fix_round="$5"
	local dirty_json git_head
	dirty_json="$(dirty_worktree_files_json)"
	git_head="$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || true)"
	if [[ -z "$git_head" ]]; then
		git_head="unborn"
	fi

	jq \
		--arg storyId "$story_id" \
		--argjson storyIteration "$story_iteration" \
		--argjson fixRound "$fix_round" \
		--arg generatedAt "$(timestamp_utc)" \
		--arg gitHead "$git_head" \
		--argjson worktreeDirtyFiles "$dirty_json" \
		'. + {
			storyId: $storyId,
			storyIteration: $storyIteration,
			fixRound: $fixRound,
			generatedAt: $generatedAt,
			gitHead: $gitHead,
			worktreeDirtyFiles: $worktreeDirtyFiles
		}' "$raw_message_path" >"$artifact_path"
}

write_synthetic_eval_infra_artifact() {
	local artifact_path="$1"
	local story_id="$2"
	local story_iteration="$3"
	local fix_round="$4"
	local reason="$5"
	local dirty_json git_head
	dirty_json="$(dirty_worktree_files_json)"
	git_head="$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || true)"
	if [[ -z "$git_head" ]]; then
		git_head="unborn"
	fi

	jq -n \
		--arg storyId "$story_id" \
		--argjson storyIteration "$story_iteration" \
		--argjson fixRound "$fix_round" \
		--arg generatedAt "$(timestamp_utc)" \
		--arg gitHead "$git_head" \
		--argjson worktreeDirtyFiles "$dirty_json" \
		--arg summary "$reason" \
		'{
			storyId: $storyId,
			storyIteration: $storyIteration,
			fixRound: $fixRound,
			generatedAt: $generatedAt,
			gitHead: $gitHead,
			worktreeDirtyFiles: $worktreeDirtyFiles,
			status: "infra_fail",
			summary: $summary,
			acceptanceCriteriaReview: [],
			findings: [],
			requiredFixes: [],
			verdictSummary: {
				decision: "infra_fail",
				primaryReason: $summary,
				requiredFixesCount: 0
			}
		}' >"$artifact_path"
}

extract_commit_json_path() {
	local eval_artifact_path="$1"
	local exec_artifact_path="$2"
	local mode
	mode="$(jq -r 'if has("approvedCommit") then "approved" else "fallback" end' "$eval_artifact_path")"
	if [[ "$mode" == "approved" ]]; then
		printf '%s\n' "$eval_artifact_path"
	else
		printf '%s\n' "$exec_artifact_path"
	fi
}

write_commit_message_file() {
	local source_json="$1"
	local message_file="$2"
	local title bullets_output
	title="$(jq -r 'if has("approvedCommit") then .approvedCommit.title else .proposedCommit.title end' "$source_json")"
	if [[ -z "$title" || "$title" == "null" ]]; then
		echo "Cannot create commit without a non-empty commit title." >&2
		return 1
	fi

	bullets_output="$(jq -r 'if has("approvedCommit") then .approvedCommit.bodyBullets[]? else .proposedCommit.bodyBullets[]? end' "$source_json")"

	{
		printf '%s\n' "$title"
		if [[ -n "$bullets_output" ]]; then
			printf '\n'
			while IFS= read -r bullet; do
				printf -- "- %s\n" "$bullet"
			done <<<"$bullets_output"
		fi
	} >"$message_file"
}

mark_story_passed() {
	local story_id="$1"
	local tmp_file
	tmp_file="$(mktemp)"
	jq --arg story_id "$story_id" '
		.userStories |= map(
			if .id == $story_id then
				. + { passes: true }
			else
				.
			end
		)
	' "$PRD_FILE" >"$tmp_file"
	mv "$tmp_file" "$PRD_FILE"
}

append_progress_entry() {
	local story_id="$1"
	local exec_artifact_path="$2"
	local eval_artifact_path="$3"
	local story_heading decision reason fix_count files_changed checks_summary learnings
	story_heading="$story_id - $(story_title "$story_id")"
	decision="$(jq -r '.verdictSummary.decision' "$eval_artifact_path")"
	reason="$(jq -r '.verdictSummary.primaryReason' "$eval_artifact_path")"
	fix_count="$(jq -r '.verdictSummary.requiredFixesCount' "$eval_artifact_path")"
	files_changed="$(jq -r 'if (.filesChanged | length) == 0 then "(none)" else (.filesChanged | join(", ")) end' "$exec_artifact_path")"
	checks_summary="$(jq -r 'if (.mechanicalChecks | length) == 0 then "(none)" else (.mechanicalChecks | map("\(.status): \(.command)") | join("; ")) end' "$exec_artifact_path")"
	learnings="$(jq -r 'if (.learnings | length) == 0 then "(none)" else (.learnings | join("; ")) end' "$exec_artifact_path")"

	{
		echo "## [$(timestamp_utc)] - $story_heading"
		echo "- What was implemented: $(jq -r '.summary' "$exec_artifact_path")"
		echo "- Validation that was run: $checks_summary"
		echo "- Files changed: $files_changed"
		echo "- semantic_eval_status: $(jq -r '.status' "$eval_artifact_path")"
		echo "- decision: $decision"
		echo "- primary_reason: $reason"
		echo "- required_fixes_count: $fix_count"
		echo "- Learnings for future iterations: $learnings"
		echo "---"
	} >>"$PROGRESS_FILE"
}

run_codex_purpose() {
	local purpose="$1"
	local prompt_file="$2"
	local run_dir="$3"
	local iteration_label="$4"
	local last_message_file="$run_dir/$iteration_label.last-message.txt"
	local temp_output="$last_message_file.run"
	local rc=0

	if "$SCRIPT_DIR/run-codex.sh" \
		--purpose "$purpose" \
		--repo-root "$ROOT_DIR" \
		--prompt-file "$prompt_file" \
		--run-dir "$run_dir" \
		--iteration "$iteration_label" >"$temp_output"; then
		rc=0
	else
		rc=$?
	fi

	if [[ -s "$temp_output" ]]; then
		echo
		echo "  Final $purpose message:"
		cat "$temp_output"
	fi
	rm -f "$temp_output"
	return "$rc"
}

process_story_iteration() {
	local story_id="$1"
	local story_iteration="$2"
	local run_dir="$3"
	local initial_phase="$4"
	local initial_fix_round="$5"
	local execution_artifact_path="$6"
	local eval_artifact_path="$7"
	local story_json phase fix_round

	story_json="$(selected_story_payload "$story_id")"
	phase="$initial_phase"
	fix_round="$initial_fix_round"

	if [[ -z "$phase" ]]; then
		phase="execute"
	fi

	if [[ "$phase" == "eval" && ( -z "$execution_artifact_path" || ! -f "$execution_artifact_path" ) ]]; then
		phase="execute"
	fi

	while true; do
		if [[ "$phase" == "execute" || "$phase" == "fix" ]]; then
			local exec_prefix exec_prompt exec_raw exec_label exec_rc exec_status prior_eval_path
			if [[ "$fix_round" -gt 0 ]]; then
				exec_label="$(printf 'iteration-%03d.fix-%02d.exec' "$story_iteration" "$fix_round")"
				prior_eval_path="$eval_artifact_path"
			else
				exec_label="$(printf 'iteration-%03d.exec' "$story_iteration")"
				prior_eval_path=""
			fi
			exec_prefix="$run_dir/$exec_label"
			exec_prompt="$exec_prefix.prompt.md"
			exec_raw="$exec_prefix.story-result.raw.json"
			execution_artifact_path="$exec_prefix.story-result.json"

			render_execution_prompt "$exec_prompt" "$story_json" "$story_iteration" "$fix_round" "$exec_raw" "$prior_eval_path"
			write_active_story_checkpoint "$story_id" "$story_iteration" "$phase" "$fix_round" "$run_dir" "$execution_artifact_path" "$eval_artifact_path"

			echo "  Running execution round for $story_id (fix_round=$fix_round)"
			if run_codex_purpose "execute" "$exec_prompt" "$run_dir" "$exec_label"; then
				exec_rc=0
			else
				exec_rc=$?
			fi

			if [[ "$exec_rc" -ne 0 ]]; then
				write_synthetic_execution_infra_artifact "$execution_artifact_path" "$story_id" "$story_iteration" "$fix_round" "execution runner failed before a trustworthy story artifact was produced"
				echo "  Execution runner failed. See $(relative_to_root "$run_dir/$exec_label.status.txt")." >&2
				return 1
			fi

			if [[ ! -f "$exec_raw" ]] || ! validate_execution_core "$exec_raw"; then
				write_synthetic_execution_infra_artifact "$execution_artifact_path" "$story_id" "$story_iteration" "$fix_round" "execution artifact missing or invalid"
				echo "  Execution artifact missing or invalid: $(relative_to_root "$exec_raw")" >&2
				return 1
			fi

			normalize_execution_artifact "$exec_raw" "$execution_artifact_path" "$story_id" "$story_iteration" "$fix_round"
			exec_status="$(jq -r '.status' "$execution_artifact_path")"

			case "$exec_status" in
			ok)
				phase="eval"
				;;
			mechanical_failed)
				echo "  Mechanical checks failed for $story_id. Stopping before semantic eval." >&2
				return 1
				;;
			infra_fail)
				echo "  Execution reported infra_fail for $story_id. Preserving state for manual inspection." >&2
				return 1
				;;
			*)
				echo "  Unsupported execution status: $exec_status" >&2
				return 1
				;;
			esac
		fi

		if [[ "$phase" == "eval" ]]; then
			local eval_attempt eval_label eval_prompt eval_message eval_rc eval_status wait_seconds
			eval_attempt=0
			while true; do
				if [[ "$fix_round" -gt 0 ]]; then
					eval_label="$(printf 'iteration-%03d.fix-%02d.eval' "$story_iteration" "$fix_round")"
				else
					eval_label="$(printf 'iteration-%03d.eval' "$story_iteration")"
				fi
				eval_prompt="$run_dir/$eval_label.prompt.md"
				eval_message="$run_dir/$eval_label.last-message.txt"
				eval_artifact_path="$run_dir/$eval_label.semantic-eval.json"

				render_eval_prompt "$eval_prompt" "$story_json" "$story_iteration" "$fix_round" "$execution_artifact_path"
				write_active_story_checkpoint "$story_id" "$story_iteration" "eval" "$fix_round" "$run_dir" "$execution_artifact_path" "$eval_artifact_path"

				echo "  Running semantic eval for $story_id (fix_round=$fix_round attempt=$((eval_attempt + 1)))"
				if run_codex_purpose "eval" "$eval_prompt" "$run_dir" "$eval_label"; then
					eval_rc=0
				else
					eval_rc=$?
				fi

				if [[ "$eval_rc" -ne 0 ]]; then
					write_synthetic_eval_infra_artifact "$eval_artifact_path" "$story_id" "$story_iteration" "$fix_round" "semantic evaluator runner failed before a trustworthy verdict was produced"
				elif [[ ! -f "$eval_message" ]] || ! validate_eval_core "$eval_message"; then
					write_synthetic_eval_infra_artifact "$eval_artifact_path" "$story_id" "$story_iteration" "$fix_round" "semantic evaluator output missing or invalid"
				else
					normalize_eval_artifact "$eval_message" "$eval_artifact_path" "$story_id" "$story_iteration" "$fix_round"
				fi

				eval_status="$(jq -r '.status' "$eval_artifact_path")"
				if [[ "$eval_status" == "infra_fail" && "$eval_attempt" -lt "$RALPH_EVAL_MAX_RETRIES" ]]; then
					eval_attempt=$((eval_attempt + 1))
					wait_seconds=$((RALPH_EVAL_RETRY_WAIT_SECONDS * eval_attempt))
					echo "  Semantic eval infra_fail. Retrying in ${wait_seconds}s..." >&2
					sleep "$wait_seconds"
					continue
				fi
				break
			done

			case "$eval_status" in
			pass)
				local commit_source commit_message_file
				commit_source="$(extract_commit_json_path "$eval_artifact_path" "$execution_artifact_path")"
				commit_message_file="$run_dir/$(printf 'iteration-%03d.commit-message.txt' "$story_iteration")"
				write_commit_message_file "$commit_source" "$commit_message_file"

				if [[ "$(dirty_worktree_count)" -eq 0 ]]; then
					echo "  No worktree changes remain for $story_id; refusing to create an empty commit." >&2
					return 1
				fi

				git -C "$ROOT_DIR" add -A -- .
				git -C "$ROOT_DIR" commit -F "$commit_message_file"
				mark_story_passed "$story_id"
				append_progress_entry "$story_id" "$execution_artifact_path" "$eval_artifact_path"
				clear_active_story_checkpoint
				return 0
				;;
			soft_fail)
				if [[ "$fix_round" -ge "$RALPH_SEMANTIC_MAX_FIX_ROUNDS" ]]; then
					echo "  Semantic eval exhausted fix rounds for $story_id." >&2
					write_active_story_checkpoint "$story_id" "$story_iteration" "fix" "$fix_round" "$run_dir" "$execution_artifact_path" "$eval_artifact_path"
					return 1
				fi
				fix_round=$((fix_round + 1))
				phase="fix"
				write_active_story_checkpoint "$story_id" "$story_iteration" "$phase" "$fix_round" "$run_dir" "$execution_artifact_path" "$eval_artifact_path"
				echo "  Semantic eval soft_fail. Entering fix round $fix_round for $story_id."
				continue
				;;
			hard_fail)
				echo "  Semantic eval hard_fail for $story_id. Preserving state for manual intervention." >&2
				write_active_story_checkpoint "$story_id" "$story_iteration" "eval" "$fix_round" "$run_dir" "$execution_artifact_path" "$eval_artifact_path"
				return 1
				;;
			infra_fail)
				echo "  Semantic eval infra_fail for $story_id after retries. Preserving state." >&2
				write_active_story_checkpoint "$story_id" "$story_iteration" "eval" "$fix_round" "$run_dir" "$execution_artifact_path" "$eval_artifact_path"
				return 1
				;;
			*)
				echo "  Unsupported eval status: $eval_status" >&2
				return 1
				;;
			esac
		fi
	done
}

show_run_banner() {
	local run_dir="$1"
	echo "Starting Ralph"
	echo "  Tool: $TOOL"
	echo "  Max iterations: $MAX_ITERATIONS"
	echo "  State dir: $(relative_to_root "$STATE_DIR")"
	echo "  Stories in PRD: $(total_story_count)"
	echo "  Pending stories: $(pending_story_count)"
	echo "  Run cap applies to this launch only; rerun Ralph if stories remain."
	echo "  Run dir: $(relative_to_root "$run_dir")"
}

main() {
	ensure_prereqs
	ensure_state_layout
	init_progress_file

	if [[ ! -f "$PRD_FILE" ]]; then
		show_missing_prd_help
		exit 1
	fi

	track_current_branch
	ensure_story_context

	if [[ "$(pending_story_count)" -eq 0 ]]; then
		if [[ -f "$ACTIVE_STORY_FILE" ]]; then
			clear_active_story_checkpoint
		fi
		echo "Ralph state already complete."
		archive_current_state "already_completed"
		echo "<promise>COMPLETE</promise>"
		echo "Progress log: $(relative_to_root "$PROGRESS_FILE")"
		exit 0
	fi

	local run_id run_dir
	run_id="$(date -u +%Y%m%dT%H%M%SZ)"
	run_dir="$RUNS_DIR/$run_id"
	mkdir -p "$run_dir"
	printf '%s\n' "$run_dir" >"$LAST_RUN_FILE"

	show_run_banner "$run_dir"

	local i resumed_existing_story=0
	for i in $(seq 1 "$MAX_ITERATIONS"); do
		local story_id story_phase fix_round execution_artifact_path eval_artifact_path

		echo
		echo "==============================================================="
		echo "  Ralph Story Iteration $i of $MAX_ITERATIONS ($TOOL)"
		echo "==============================================================="
		echo "  Pending stories before story iteration: $(pending_story_count)"

		if [[ -f "$ACTIVE_STORY_FILE" && "$resumed_existing_story" -eq 0 ]]; then
			story_id="$(jq -r '.storyId' "$ACTIVE_STORY_FILE")"
			story_phase="$(jq -r '.phase' "$ACTIVE_STORY_FILE")"
			fix_round="$(jq -r '.fixRound // 0' "$ACTIVE_STORY_FILE")"
			execution_artifact_path="$(jq -r '.executionArtifactPath // ""' "$ACTIVE_STORY_FILE")"
			eval_artifact_path="$(jq -r '.evalArtifactPath // ""' "$ACTIVE_STORY_FILE")"
			resumed_existing_story=1
			if ! story_exists_pending "$story_id"; then
				echo "  Active story checkpoint is stale for $story_id; clearing it."
				clear_active_story_checkpoint
				story_id=""
			else
				echo "  Resuming active story checkpoint: $story_id (phase=$story_phase fix_round=$fix_round)"
			fi
		else
			story_id="$(next_pending_story_id)"
			if [[ -z "$story_id" ]]; then
				echo
				archive_current_state "completed"
				echo "Ralph completed all tasks."
				echo "<promise>COMPLETE</promise>"
				echo "Completed at story iteration $i of $MAX_ITERATIONS"
				echo "Progress log: $(relative_to_root "$PROGRESS_FILE")"
				echo "Run logs: $(relative_to_root "$run_dir")"
				exit 0
			fi
			story_phase="execute"
			fix_round=0
			execution_artifact_path=""
			eval_artifact_path=""
			echo "  Selected story: $story_id - $(story_title "$story_id")"
		fi

		if [[ -z "$story_id" ]]; then
			story_id="$(next_pending_story_id)"
			story_phase="execute"
			fix_round=0
			execution_artifact_path=""
			eval_artifact_path=""
			if [[ -z "$story_id" ]]; then
				continue
			fi
			echo "  Selected story: $story_id - $(story_title "$story_id")"
		fi

		if ! process_story_iteration "$story_id" "$i" "$run_dir" "$story_phase" "$fix_round" "$execution_artifact_path" "$eval_artifact_path"; then
			echo "Ralph stopped while processing story $story_id." >&2
			echo "Progress log: $(relative_to_root "$PROGRESS_FILE")" >&2
			echo "Run logs: $(relative_to_root "$run_dir")" >&2
			exit 1
		fi

		if [[ "$(pending_story_count)" -eq 0 ]]; then
			echo
			archive_current_state "completed"
			echo "Ralph completed all tasks."
			echo "<promise>COMPLETE</promise>"
			echo "Completed at story iteration $i of $MAX_ITERATIONS"
			echo "Progress log: $(relative_to_root "$PROGRESS_FILE")"
			echo "Run logs: $(relative_to_root "$run_dir")"
			exit 0
		fi

		echo "Story iteration $i complete. Continuing..."
		sleep 2
	done

	echo
	echo "Ralph reached max iterations ($MAX_ITERATIONS) with $(pending_story_count) pending stories still in $(relative_to_root "$PRD_FILE")."
	echo "This run cap applies to stories in this launch only."
	echo "It does not limit how many stories may exist in prd.json."
	echo "Rerun \`just ralph\` to continue, or pass a larger iteration cap for this launch."
	echo "Progress log: $(relative_to_root "$PROGRESS_FILE")"
	echo "Run logs: $(relative_to_root "$run_dir")"
	exit 1
}

main "$@"
