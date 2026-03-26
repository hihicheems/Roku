# Contributing

Thank you for contributing to [PROJECT_NAME].

This document is a reusable template. Copy it to `CONTRIBUTING.md` and replace bracketed placeholders with project-specific details.

## Ways To Contribute

Contributions may include:

- bug reports
- feature requests
- documentation improvements
- tests
- code changes
- maintenance and tooling updates

## Before Opening An Issue

- Use the issue form or template that best matches your request.
- Search existing issues first when possible.
- Keep each issue focused on one problem, request, or question.
- Include enough context for another contributor to understand the situation.

## Before Opening A Pull Request

- Keep the change focused and reviewable.
- Split unrelated work into separate PRs when practical.
- Link the relevant issue for non-trivial changes.
- Describe what changed, why it changed, and how it was validated.
- Call out migration, compatibility, security, or rollout concerns when they exist.

## Validation Expectations

Prefer the smallest relevant validation that matches the change surface.

- Localized changes: focused checks and targeted tests are usually enough.
- Shared or higher-risk changes: run broader checks before handoff.
- If the project has standard verification commands, list them here:
  - `[format command]`
  - `[lint command]`
  - `[test command]`

Report only what you actually ran.

## Commit Messages

Document the project's commit message convention here.

If the project uses Conventional Commits, a common format is:

```text
type(scope): description
```

Example types:

- `feat`
- `fix`
- `docs`
- `refactor`
- `test`
- `chore`
- `build`
- `ci`
- `revert`

Suggested guidance:

- keep the description short and imperative
- add a scope when it improves clarity
- describe breaking changes clearly
- align squash commit titles with the same convention when possible

## Pull Request Reviews

Reviewers should be able to answer these questions quickly:

- What changed?
- Why is it needed?
- How was it checked?
- What issue does it relate to?
- What risk remains?

## Communication

Add any project-specific guidance here, for example:

- where to ask questions
- expected response times
- review ownership
- release or deployment coordination rules
