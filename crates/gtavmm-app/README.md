# gtavmm-app

Desktop app for GTAV Mods Manager, built with Tauri v2 (Rust backend) and React 19 + TypeScript + Vite (frontend).

This crate contains no business logic. The Tauri command layer in `src-tauri/src/commands.rs` is a thin wrapper over `gtavmm-core`, which is shared with `gtavmm-cli`.

## Layout

- `src/pages` — workspace pages (Legacy/Enhanced SP and LSPDFR, FiveM, Settings, Tools, Profiles, DLL translation, activity log, saved links).
- `src/components` — shared UI components (install wizard, mod table, detail drawer, sidebar).
- `src/i18n` — i18next setup and locale files (`en`, `zh-TW`).
- `src-tauri` — Rust crate `gtavmm-app`: command registration (`src/lib.rs`), commands, Tauri configuration and capabilities.

## Development

From this directory:

```
npm ci
npm run dev      # Vite dev server on http://localhost:1420
npm run build    # tsc + vite build (also used by CI)
```

From the repository root:

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

See [CONTRIBUTING.md](../../CONTRIBUTING.md) for the full workflow. Code license: PolyForm Noncommercial License 1.0.0.

<!-- Anthropic-Claude_Code_Responsible: Source_Code -->
