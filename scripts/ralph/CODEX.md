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
3. If the validation passes, commit the changes with:
   `feat: [Story ID] - [Story Title]`
4. Update the PRD so that the completed story has `passes: true`.
5. Append a short progress entry to the progress log.

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
- Do not make speculative wide-scope refactors.
- Keep CI-friendly behavior and preserve existing repo conventions.

## Completion Signal

After finishing a story, check the PRD again.

- If all stories are now complete, reply exactly with `<promise>COMPLETE</promise>`.
- Otherwise, finish normally; Ralph will start another fresh Codex iteration later.
