Journey-Step: #<issue-number>
Closing-Issues:
- Closes #<issue-number>
Executor: <Codex CLI | Claude Code | Cursor>

## Plan

- What will change and why.
- How the work maps to the issue scope and acceptance criteria.

## Outcome

- What changed.
- How the outcome satisfies each acceptance criterion.

## Files / Modules touched

- Major files, modules, crates, packages, or config files changed.

## Validation

- Formatting checks run and result.
- Lint/static-analysis checks run and result.
- Tests run and result.
- CI status or explicit reason CI is not applicable.

## Risk

- Risk level.
- Mitigations, rollback notes, or follow-up required.

## Notes

- Reviewer context, limitations, or intentionally deferred work.

## Repository validation evidence

Include results for the applicable repo gates:
- `cargo test`
- `cargo check`
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `ruff check .`
- `ruff format --check .`
- `python -m pytest`

Gap code: `rop.pr_review_expectations`
