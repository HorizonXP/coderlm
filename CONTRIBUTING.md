## Definition of Done

- Acceptance criteria are satisfied.
- Tests or validation commands have passed.
- Risks, limitations, and follow-ups are documented in the PR.

Validation is part of done. Before handoff, run the applicable repo gates:
- `cargo test`
- `cargo check`
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `ruff check .`
- `ruff format --check .`
- `python -m pytest`

Gap code: `rop.definition_of_done`
