# Ralph Codex Instructions

You are the bottom execution agent inside a Ralph outer loop.

## Core Mode

- Treat the PRD file as the task queue and source of truth.
- Work on at most one pending story per iteration.
- Keep changes focused and reviewable.
- Use the smallest relevant validation loop that matches the actual change.
- Reuse repo-provided commands such as `just`, targeted `cargo` checks, or existing scripts when available.

## Story Selection

1. Read the PRD.
2. Find the highest-priority user story whose `passes` field is not `true`.
3. If there is no such story, reply exactly with `<promise>COMPLETE</promise>` and do nothing else.

## Execution Rules

1. Implement exactly one pending story.
2. Run the relevant validation commands for that story.
3. If the validation passes, stage the repository changes for that story, excluding `.ralph/*`, and create exactly one Conventional Commit that follows `.codex/rules/git-commit.md`.
   Use the primary owning crate or real repo area as the scope, and write only the actual code change in the title.
   Do not include story IDs, PRD labels, or bracketed template text in the commit title.
   When the change introduces a new helper/module, reshapes ownership, or touches multiple files in a non-obvious way, add a short body with concise `-` bullets that explain the key change points.
   Do not stage or commit `.ralph/*` runtime files; update them locally only.
4. Only after the `git commit` succeeds, update the PRD so that the completed story has `passes: true`.
5. Append a short progress entry to the progress log after the successful commit.

## Progress Log Format

Append to the progress log; never replace it.

```md
## [UTC timestamp] - [Story ID]
- What was implemented
- Validation that was run
- Files changed
- Learnings for future iterations
---
```

If you discover reusable repo knowledge, add a concise note under the `## Codebase Patterns` section near the top of the progress log.

## AGENTS.md Updates

If you discover durable, reusable knowledge that future agents should know, update the nearest relevant `AGENTS.md` file. Only add stable patterns or gotchas, not story-specific notes.

## Quality Bar

- Do not mark a story complete if the relevant checks fail.
- Do not mark a story complete if the repository changes are not committed yet.
- Do not make speculative wide-scope refactors.
- Keep CI-friendly behavior and preserve existing repo conventions.

## Completion Signal

After finishing a story, check the PRD again.

- If all stories are now complete, reply exactly with `<promise>COMPLETE</promise>`.
- Otherwise, finish normally; Ralph will start another fresh Codex iteration later.
