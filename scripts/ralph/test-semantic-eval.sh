#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_SOURCE_DIR="$ROOT_DIR/scripts/ralph"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

assert_file() {
	local path="$1"
	[[ -f "$path" ]] || fail "missing file: $path"
}

assert_eq() {
	local expected="$1"
	local actual="$2"
	local message="$3"
	if [[ "$expected" != "$actual" ]]; then
		fail "$message (expected=$expected actual=$actual)"
	fi
}

make_mock_codex() {
	local path="$1"
	cat >"$path" <<'EOF'
#!/usr/bin/env bash

set -euo pipefail

LAST_MESSAGE_FILE=""
REPO_ROOT=""

while [[ $# -gt 0 ]]; do
	case "$1" in
	-o)
		LAST_MESSAGE_FILE="$2"
		shift 2
		;;
	-C)
		REPO_ROOT="$2"
		shift 2
		;;
	*)
		shift
		;;
	esac
done

PROMPT="$(cat)"
PURPOSE="execute"
if grep -q "Ralph Semantic Eval Context" <<<"$PROMPT"; then
	PURPOSE="eval"
fi

mkdir -p "$REPO_ROOT/.mock-state"

extract_bullet_after() {
	local marker="$1"
	printf '%s\n' "$PROMPT" | awk -v marker="$marker" '
		$0 == marker { getline; sub(/^- /, ""); print; exit }
	'
}

extract_story_id() {
	printf '%s\n' "$PROMPT" | grep -o '"id":"[^"]*"' | head -n1 | cut -d'"' -f4
}

write_execution_artifact() {
	local path="$1"
	local status="$2"
	local summary="$3"
	local files_json="$4"
	local checks_json="$5"
	local claims_json="$6"
	local title="$7"
	local bullets_json="$8"
	local learnings_json="$9"
	jq -n \
		--arg status "$status" \
		--arg summary "$summary" \
		--arg title "$title" \
		--argjson filesChanged "$files_json" \
		--argjson mechanicalChecks "$checks_json" \
		--argjson acceptanceCriteriaClaims "$claims_json" \
		--argjson bodyBullets "$bullets_json" \
		--argjson learnings "$learnings_json" \
		'{
			status: $status,
			summary: $summary,
			filesChanged: $filesChanged,
			mechanicalChecks: $mechanicalChecks,
			acceptanceCriteriaClaims: $acceptanceCriteriaClaims,
			proposedCommit: {
				title: $title,
				bodyBullets: $bodyBullets
			},
			learnings: $learnings
		}' >"$path"
}

write_eval_message() {
	local status="$1"
	local summary="$2"
	local reviews_json="$3"
	local findings_json="$4"
	local fixes_json="$5"
	local decision="$6"
	local reason="$7"
	local fix_count="$8"
	jq -n \
		--arg status "$status" \
		--arg summary "$summary" \
		--arg decision "$decision" \
		--arg reason "$reason" \
		--argjson acceptanceCriteriaReview "$reviews_json" \
		--argjson findings "$findings_json" \
		--argjson requiredFixes "$fixes_json" \
		--argjson requiredFixesCount "$fix_count" \
		'{
			status: $status,
			summary: $summary,
			acceptanceCriteriaReview: $acceptanceCriteriaReview,
			findings: $findings,
			requiredFixes: $requiredFixes,
			verdictSummary: {
				decision: $decision,
				primaryReason: $reason,
				requiredFixesCount: $requiredFixesCount
			}
		}' >"$LAST_MESSAGE_FILE"
}

STORY_ID="$(extract_story_id)"
EXEC_ARTIFACT="$(extract_bullet_after "Write the execution artifact JSON to:")"
SCENARIO="${RALPH_TEST_SCENARIO:-pass}"

printf '{"event":"mock","purpose":"%s","scenario":"%s"}\n' "$PURPOSE" "$SCENARIO"

case "$SCENARIO:$PURPOSE" in
pass:execute)
	echo "pass scenario" >"$REPO_ROOT/story.txt"
	write_execution_artifact \
		"$EXEC_ARTIFACT" \
		"ok" \
		"implemented pass scenario" \
		'["story.txt"]' \
		'[{"command":"echo pass","status":"passed"}]' \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","claimedStatus":"met","evidence":"echo pass"}]' \
		'fix(scripts): pass scenario' \
		'["write story.txt"]' \
		'["keep artifacts explicit"]'
	printf 'execution done\n' >"$LAST_MESSAGE_FILE"
	;;
pass:eval)
	write_eval_message \
		"pass" \
		"implementation satisfies the story" \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","judgment":"met","evidence":"echo pass"}]' \
		'[]' \
		'[]' \
		"pass" \
		"criteria met" \
		0
	;;
soft_fix:execute)
	if [[ -f "$REPO_ROOT/.mock-state/soft-fix-ready" ]]; then
		echo "good" >"$REPO_ROOT/story.txt"
	else
		echo "bad" >"$REPO_ROOT/story.txt"
	fi
	write_execution_artifact \
		"$EXEC_ARTIFACT" \
		"ok" \
		"implemented soft-fix scenario" \
		'["story.txt"]' \
		'[{"command":"echo soft-fix","status":"passed"}]' \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","claimedStatus":"met","evidence":"echo soft-fix"}]' \
		'fix(scripts): soft fix scenario' \
		'["update story.txt"]' \
		'["follow eval guidance"]'
	printf 'execution done\n' >"$LAST_MESSAGE_FILE"
	;;
soft_fix:eval)
	if grep -q '^bad$' "$REPO_ROOT/story.txt"; then
		touch "$REPO_ROOT/.mock-state/soft-fix-ready"
		write_eval_message \
			"soft_fail" \
			"story still contains bad content" \
			'[{"criterionId":"AC-1","criterionText":"Typecheck passes","judgment":"met","evidence":"echo soft-fix"}]' \
			'["story.txt still says bad"]' \
			'["Replace bad with good in story.txt"]' \
			"soft_fail" \
			"story content is not yet corrected" \
			1
	else
		write_eval_message \
			"pass" \
			"fix round corrected the story" \
			'[{"criterionId":"AC-1","criterionText":"Typecheck passes","judgment":"met","evidence":"story.txt says good"}]' \
			'[]' \
			'[]' \
			"pass" \
			"fix applied" \
			0
	fi
	;;
eval_infra_retry:execute)
	echo "stable" >"$REPO_ROOT/story.txt"
	write_execution_artifact \
		"$EXEC_ARTIFACT" \
		"ok" \
		"implemented eval infra retry scenario" \
		'["story.txt"]' \
		'[{"command":"echo stable","status":"passed"}]' \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","claimedStatus":"met","evidence":"echo stable"}]' \
		'fix(scripts): eval infra retry scenario' \
		'["write stable story"]' \
		'["retry evaluator on infra fail"]'
	printf 'execution done\n' >"$LAST_MESSAGE_FILE"
	;;
eval_infra_retry:eval)
	COUNT_FILE="$REPO_ROOT/.mock-state/eval-infra-count"
	COUNT=0
	if [[ -f "$COUNT_FILE" ]]; then
		COUNT="$(cat "$COUNT_FILE")"
	fi
	COUNT=$((COUNT + 1))
	printf '%s\n' "$COUNT" >"$COUNT_FILE"
	if [[ "$COUNT" -eq 1 ]]; then
		printf 'not-json\n' >"$LAST_MESSAGE_FILE"
	else
		write_eval_message \
			"pass" \
			"evaluator recovered after infra failure" \
			'[{"criterionId":"AC-1","criterionText":"Typecheck passes","judgment":"met","evidence":"echo stable"}]' \
			'[]' \
			'[]' \
			"pass" \
			"second evaluator attempt succeeded" \
			0
	fi
	;;
adopt:execute)
	if [[ ! -f "$REPO_ROOT/dirty.txt" ]]; then
		printf 'dirty\n' >"$REPO_ROOT/dirty.txt"
	fi
	write_execution_artifact \
		"$EXEC_ARTIFACT" \
		"ok" \
		"adopted existing dirty worktree" \
		'["dirty.txt"]' \
		'[{"command":"echo adopt","status":"passed"}]' \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","claimedStatus":"met","evidence":"echo adopt"}]' \
		'fix(scripts): adopt dirty worktree scenario' \
		'["commit adopted worktree"]' \
		'["require explicit adoption"]'
	printf 'execution done\n' >"$LAST_MESSAGE_FILE"
	;;
adopt:eval)
	write_eval_message \
		"pass" \
		"adopted worktree satisfies the story" \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","judgment":"met","evidence":"echo adopt"}]' \
		'[]' \
		'[]' \
		"pass" \
		"dirty worktree was adopted intentionally" \
		0
	;;
mechanical_failed:execute)
	printf 'broken\n' >"$REPO_ROOT/story.txt"
	write_execution_artifact \
		"$EXEC_ARTIFACT" \
		"mechanical_failed" \
		"mechanical checks failed" \
		'["story.txt"]' \
		'[{"command":"false","status":"failed"}]' \
		'[{"criterionId":"AC-1","criterionText":"Typecheck passes","claimedStatus":"not_met","evidence":"false"}]' \
		'' \
		'[]' \
		'["stop before semantic eval"]'
	printf 'execution failed mechanically\n' >"$LAST_MESSAGE_FILE"
	;;
mechanical_failed:eval)
	write_eval_message \
		"hard_fail" \
		"should not run" \
		'[]' \
		'[]' \
		'[]' \
		"hard_fail" \
		"semantic eval should not run after mechanical failure" \
		0
	;;
*)
	printf 'unsupported mock scenario: %s (%s)\n' "$SCENARIO" "$PURPOSE" >&2
	exit 1
	;;
esac
EOF
	chmod +x "$path"
}

setup_temp_repo() {
	local repo_dir="$1"
	mkdir -p "$repo_dir/scripts/ralph"
	cp "$SCRIPT_SOURCE_DIR/ralph.sh" "$repo_dir/scripts/ralph/ralph.sh"
	cp "$SCRIPT_SOURCE_DIR/run-codex.sh" "$repo_dir/scripts/ralph/run-codex.sh"
	cp "$SCRIPT_SOURCE_DIR/CODEX.md" "$repo_dir/scripts/ralph/CODEX.md"
	cp "$SCRIPT_SOURCE_DIR/EVAL.md" "$repo_dir/scripts/ralph/EVAL.md"
	printf '.ralph/\n' >"$repo_dir/.gitignore"
	printf '# temp repo\n' >"$repo_dir/README.md"
	make_mock_codex "$repo_dir/mock-codex.sh"
	(
		cd "$repo_dir"
		git init -q
		git config user.name "Ralph Test"
		git config user.email "ralph-test@example.com"
		git add README.md .gitignore scripts/ralph mock-codex.sh
		git commit -q -m "chore: initialize test repo"
	)
}

write_prd() {
	local repo_dir="$1"
	local story_title="$2"
	mkdir -p "$repo_dir/.ralph"
	cat >"$repo_dir/.ralph/prd.json" <<EOF
{
  "project": "Ralph Semantic Eval Test",
  "branchName": "ralph/test",
  "description": "Test harness for Ralph semantic eval",
  "userStories": [
    {
      "id": "US-001",
      "title": "$story_title",
      "description": "As a test, I want a single story to execute.",
      "acceptanceCriteria": [
        "Typecheck passes"
      ],
      "priority": 1,
      "passes": false,
      "notes": ""
    }
  ]
}
EOF
}

run_case_pass() {
	local tmp_dir="$1/pass"
	setup_temp_repo "$tmp_dir"
	write_prd "$tmp_dir" "pass story"
	(
		cd "$tmp_dir"
		RALPH_CODEX_BIN="$tmp_dir/mock-codex.sh" \
		RALPH_TEST_SCENARIO="pass" \
		./scripts/ralph/ralph.sh --state-dir .ralph 1
	)
	assert_eq "true" "$(jq -r '.userStories[0].passes' "$tmp_dir/.ralph/prd.json")" "pass scenario should mark story complete"
	assert_eq "2" "$(git -C "$tmp_dir" rev-list --count HEAD)" "pass scenario should create one story commit"
	assert_file "$tmp_dir/.ralph/runs/$(basename "$(cat "$tmp_dir/.ralph/.last-run")")/iteration-001.exec.story-result.json"
	assert_file "$tmp_dir/.ralph/runs/$(basename "$(cat "$tmp_dir/.ralph/.last-run")")/iteration-001.eval.semantic-eval.json"
}

run_case_soft_fix() {
	local tmp_dir="$1/soft-fix"
	setup_temp_repo "$tmp_dir"
	write_prd "$tmp_dir" "soft fix story"
	(
		cd "$tmp_dir"
		RALPH_CODEX_BIN="$tmp_dir/mock-codex.sh" \
		RALPH_TEST_SCENARIO="soft_fix" \
		./scripts/ralph/ralph.sh --state-dir .ralph 1
	)
	assert_eq "true" "$(jq -r '.userStories[0].passes' "$tmp_dir/.ralph/prd.json")" "soft-fix scenario should mark story complete"
	assert_file "$tmp_dir/.ralph/runs/$(basename "$(cat "$tmp_dir/.ralph/.last-run")")/iteration-001.fix-01.exec.story-result.json"
	assert_eq "good" "$(cat "$tmp_dir/story.txt")" "soft-fix scenario should apply evaluator guidance"
}

run_case_eval_infra_retry() {
	local tmp_dir="$1/eval-infra-retry"
	setup_temp_repo "$tmp_dir"
	write_prd "$tmp_dir" "eval infra retry story"
	(
		cd "$tmp_dir"
		RALPH_CODEX_BIN="$tmp_dir/mock-codex.sh" \
		RALPH_TEST_SCENARIO="eval_infra_retry" \
		RALPH_EVAL_MAX_RETRIES=2 \
		./scripts/ralph/ralph.sh --state-dir .ralph 1
	)
	assert_eq "2" "$(cat "$tmp_dir/.mock-state/eval-infra-count")" "eval infra retry should rerun evaluator"
	assert_eq "true" "$(jq -r '.userStories[0].passes' "$tmp_dir/.ralph/prd.json")" "eval infra retry should eventually pass"
}

run_case_adopt() {
	local tmp_dir="$1/adopt"
	setup_temp_repo "$tmp_dir"
	write_prd "$tmp_dir" "adopt dirty worktree story"
	printf 'dirty\n' >"$tmp_dir/dirty.txt"
	(
		cd "$tmp_dir"
		RALPH_CODEX_BIN="$tmp_dir/mock-codex.sh" \
		RALPH_TEST_SCENARIO="adopt" \
		./scripts/ralph/ralph.sh --state-dir .ralph --adopt-dirty-worktree US-001 --yes 1
	)
	assert_eq "true" "$(jq -r '.userStories[0].passes' "$tmp_dir/.ralph/prd.json")" "adopt scenario should mark story complete"
	assert_file "$tmp_dir/dirty.txt"
}

run_case_mechanical_failed() {
	local tmp_dir="$1/mechanical-failed"
	setup_temp_repo "$tmp_dir"
	write_prd "$tmp_dir" "mechanical failed story"
	set +e
	(
		cd "$tmp_dir"
		RALPH_CODEX_BIN="$tmp_dir/mock-codex.sh" \
		RALPH_TEST_SCENARIO="mechanical_failed" \
		./scripts/ralph/ralph.sh --state-dir .ralph 1
	)
	local rc=$?
	set -e
	assert_eq "1" "$rc" "mechanical_failed scenario should stop"
	assert_eq "false" "$(jq -r '.userStories[0].passes' "$tmp_dir/.ralph/prd.json")" "mechanical_failed scenario should not mark story complete"
	assert_eq "1" "$(git -C "$tmp_dir" rev-list --count HEAD)" "mechanical_failed scenario should not commit"
}

main() {
	local tmp_root
	tmp_root="$(mktemp -d)"
	trap "rm -rf '$tmp_root'" EXIT

	run_case_pass "$tmp_root"
	run_case_soft_fix "$tmp_root"
	run_case_eval_infra_retry "$tmp_root"
	run_case_adopt "$tmp_root"
	run_case_mechanical_failed "$tmp_root"

	echo "Ralph semantic eval harness tests passed."
}

main "$@"
