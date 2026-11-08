# Contributing to JetSearch

Thanks for taking an interest in JetSearch.

## Before you start

- Check existing [issues](https://github.com/harshit-nimje/JetSearch/issues) before starting significant work.
- For larger changes, open an issue first so the proposed direction can be discussed.
- Keep pull requests focused. Separate refactors from behavior changes where practical.

## Development setup

1. Install the stable Rust toolchain with [rustup](https://rustup.rs/).
2. Clone the repository.
3. Run the application with `cargo run`.
4. Run the test suite with `cargo test --locked`.

## Code guidelines

- Follow standard Rust formatting. Run `cargo fmt --all -- --check` before submitting.
- Prefer clear, maintainable code over clever abstractions.
- Avoid adding dependencies unless they provide a clear benefit.
- Keep indexing and searching responsive; avoid blocking the UI thread with expensive work.
- Handle filesystem errors gracefully. Users may not have permission to read every directory.
- Add or update tests when changing query parsing, filtering, ranking, or filesystem traversal.

## Pull requests

A good pull request includes:

- A concise description of the problem and solution.
- The motivation and relevant context.
- Tests run and their results.
- Screenshots or a short recording for visible UI changes.
- Known limitations or follow-up work.

By submitting a contribution, you agree that it may be distributed under the project's eventual license. The repository currently has no declared license, so please open a discussion before contributing code that depends on a particular licensing arrangement.
