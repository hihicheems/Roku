# Contributing

Thank you for contributing to Roku.

This repository prefers small, reviewable changes with clear rationale and honest validation notes.

## Before Opening An Issue

- Use the issue form that best matches your request.
- Search existing issues first when possible.
- Keep reports concrete and scoped to one problem or request.

## Before Opening A Pull Request

- Keep the change focused. Split unrelated work into separate PRs when possible.
- Link the relevant issue for non-trivial changes.
- Describe what changed, why it changed, and how it was validated.
- Call out migration, compatibility, or rollout concerns when they exist.

## Validation Expectations

Prefer the smallest relevant validation that matches the change surface.

- Localized changes: focused checks and targeted tests are usually enough.
- Shared or higher-risk changes: run broader checks before handoff.
- When the workspace-wide gate is needed, the common commands are:
  - `just fmt`
  - `just lint`
  - `just test`

Report only what you actually ran.

## Commit Messages

This repository follows Conventional Commits.

Use the format:

```text
type(scope): description
```

Examples:

- `feat(roku-runtime-service): add health probe for memory provider`
- `fix(scripts): harden dev service startup`
- `docs(workspace): clarify local validation flow`

Guidelines:

- Use lowercase commit types such as `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `build`, `ci`, or `revert`.
- Keep the description short and imperative.
- Add a scope when it improves clarity.
- Prefer the owning crate name or top-level repo area as the scope when helpful.
- Use `!` or a `BREAKING CHANGE:` footer for breaking changes.

If your PR will be squash merged, prefer a PR title or suggested squash title that already follows this format.

## Pull Request Reviews

Reviewers should be able to answer these questions quickly:

- What changed?
- Why is it needed?
- How was it checked?
- What issue does it relate to?
- What risk remains?
