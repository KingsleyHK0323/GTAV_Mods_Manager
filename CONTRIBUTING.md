# Contributing

This project is currently maintained by a single author (with AI-assisted
development). It's not actively recruiting contributors yet, but the repository is
source-available under the PolyForm Noncommercial License 1.0.0, and external pull
requests are welcome — reviewed at the maintainer's discretion, since there's no formal
review team yet.

## Development setup

```
cargo build
cargo test
cargo run -p gtavmm-cli -- --help
```

<!-- Updated by OpenAI-Codex: Document GitHub Actions validation. -->

GitHub Actions runs the shared CI workflow on hosted Windows and Ubuntu runners for pushes to
`Source_Code` and pull requests. It checks the Rust workspace and builds the frontend. For local
validation, the workflow commands are:

```
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

From the repository root, run the frontend checks locally with:

```
cd crates/gtavmm-app
npm ci
npm run build
```

## Project structure

- `crates/gtavmm-core` — all business logic. No UI/CLI dependencies. This is where
  most functional changes belong.
- `crates/gtavmm-cli` — CLI wrapper over `gtavmm-core`.
- `crates/gtavmm-app` — placeholder for the future Tauri + React UI (not yet
  scaffolded — see its `README.md`).

Adding support for a new mode (Enhanced SP, LSPDFR, FiveM, etc.) should mean writing a
new `ModeProvider` implementation (see `crates/gtavmm-core/src/providers/mod.rs`), not
modifying `mod_analyzer`'s classification logic.

## License and attribution

By submitting a contribution, you agree it will be licensed under this project's
[PolyForm Noncommercial License 1.0.0](LICENSE), subject to any separate third-party
license notices that apply to contributed files.

If you publish a modified version or a derivative work of this project, please
**conspicuously credit** it — e.g. in your README's opening section or an in-app
About/Credits screen — along the lines of: "Based on GTAV Mods Manager, original
author: SpaceSquare; licensor: Kingsley." This is a project convention layered on top
of (not a replacement for) the PolyForm license's notice-preservation requirements.

## Reporting issues

Open an issue on [this repository](https://github.com/SpaceSquare640/GTAV_Mods_Manager/issues).
There's no separate security-disclosure process yet — for anything sensitive, say so
in the issue and we'll figure out a private channel.
