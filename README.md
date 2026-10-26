# GTAV Mods Manager

A cross-platform (Windows + Linux) mod manager for Grand Theft Auto V, covering Legacy and
Enhanced single-player mods, LSPDFR, and FiveM.

## Status

<!-- Updated by OpenAI-Codex: FiveM Client installed-mod list is now connected. -->
<!-- Updated by OpenAI-Codex: FiveM Client rows now expose the mod detail drawer. -->
<!-- Updated by OpenAI-Codex: FiveM Client now has a mode-filtered recycle bin. -->
<!-- Updated by OpenAI-Codex: FiveM Client folder path is saved locally. -->
<!-- Updated by OpenAI-Codex: recycle-bin restores use recorded roots and preserve incomplete snapshots. -->
<!-- OpenAI-Codex: Translation supports provider-specific AI adapters and refuses output collisions. -->
<!-- OpenAI-Codex: GTA install previews and installs require a configured path and reject edition mismatches. -->
<!-- OpenAI-Codex: Page lifecycle actions and SP/LSPDFR tools use the configured path and verify the page edition. -->
<!-- OpenAI-Codex: Legacy SP spreadsheet exports are limited to that page's records. -->
<!-- OpenAI-Codex: FiveM Server is temporarily unavailable in the desktop App. -->

The **core engine (`gtavmm-core`) and CLI (`gtavmm-cli`) are functional and tested** — install,
uninstall, enable/disable, conflict detection, recycle bin, full backups, multi-profile
switching, FiveM resource load-order resolution, an SP→FiveM add-on vehicle converter, and an
opt-in local/cloud AI assistant (crash-log diagnosis, translation drafts, a known-fix rule
library, and a Plan → confirm → execute action layer) are all implemented against real mod
samples, not just synthetic tests.
The provider-specific AI adapters described below have only received static review in this
development pass; live API calls have not been verified.

The **desktop app (`gtavmm-app`, Tauri + React) is under active development, not yet feature
complete**. It has a working shell (sidebar navigation, i18n with English/Traditional Chinese)
and pages wired to backend commands, including Legacy SP mod listing; FiveM Client mod listing
with detail, lifecycle controls, a mode-filtered recycle bin, and a locally remembered client
folder; the FiveM Server load-order page, which remains in source but is temporarily unavailable
in the App; and the SP→FiveM converter. Legacy SP and Legacy
LSPDFR support batch enable/disable from the installed-mod list. GTA SP/LSPDFR previews and
installs require a configured game path and refuse page/edition mismatches instead of falling
back to automatic detection.
LSPDFR folders and archives with an explicit `Plugins/LSPDFR` game-root layout preserve that
path, including packages wrapped in one `Grand Theft Auto V` folder. Packages containing DLLs
without a recognized layout are refused as ambiguous; standalone managed DLLs still target
`Plugins/` and may need to be moved to `Plugins/LSPDFR/` when they are callouts.
First-run setup saves the detected install unless you choose another folder. Reinstall and
uninstall actions, component checks, and full backup/restore use the configured path and validate
the active page's edition before acting. App-created full backups record their GTA V edition;
the App lists and restores only matching-edition archives and reports skipped unmarked or damaged
archives. Older unmarked archives are rejected by App restore because their source edition cannot
be verified. The Legacy SP Excel export includes only Legacy SP installed and uninstalled records.
<!-- OpenAI-Codex_Responsible: Source_Code: generated INI/XML drafts use quick-xml 0.36.2 structural checks. -->
The Tools editor can generate AI translation drafts from saved INI/XML files. The core rejects
drafts that change INI section or key syntax, contain XML parse errors, or change XML element,
attribute, namespace binding, or nesting structure. Custom XML entity references in element text
must remain unchanged; predefined and numeric character references are excluded from the structural
signature. The original file and any existing draft remain unchanged when validation fails. Indented
INI lines that look like assignments or section headers remain structural; the validator does not
guess mod-specific continuation syntax.
The translation tools support reviewable DLL, INI, and XML output through Ollama Local, Ollama Cloud,
OpenAI, Anthropic, DeepSeek, OpenRouter, Google AI, or NVIDIA AI. Provider API keys are stored
separately in the operating system's credential store. Translation inputs are sent unchanged and may
include local paths or other sensitive values; diagnosis de-identification does not apply to
translation. Cloud translation may incur usage charges. Source files and existing translation outputs
are not overwritten. Config translation requires UTF-8-readable files and rejects XML declarations
that specify another encoding before contacting a provider. DLL translations are bound to the
inspected source file, discard pending results after the selected file or target language changes,
and default translated DLLs remain beside their source files.

Other workflows remain in progress — the CLI remains the most complete way to use the tool today. See the
[Wiki](https://github.com/SpaceSquare640/GTAV_Mods_Manager/wiki) for a full breakdown of what's
implemented vs. planned.

Recycle-bin entries record their target root and the exact files they snapshot. Restore keeps the
entry intact when a recorded snapshot is missing.

The installer can extract unencrypted, single-volume RAR 1.5+ archives for Legacy SP,
Legacy LSPDFR, and FiveM Client when entry names are UTF-8 or carry Unicode name metadata.
RAR 1.3/1.4, legacy names that require an unspecified code page, symbolic links, redirected
entries, and multi-volume archives are rejected. Extraction uses a temporary staging
directory with path validation and limits of 10,000 entries, 2 GiB per file, and 8 GiB total;
archives outside those limits must be extracted manually before installation.

## License

**Software:** PolyForm Noncommercial License 1.0.0 (SPDX: `PolyForm-Noncommercial-1.0.0`).
The licensor is Kingsley; see [LICENSE](LICENSE) for the terms and required notice.

**Project-authored bundled data and documentation:** PolyForm Noncommercial License 1.0.0,
unless a file states otherwise. Third-party content retains its own license and notices.

## Author

Kingsley

## Repository

https://github.com/SpaceSquare640/GTAV_Mods_Manager

**This is the only official distribution channel.** Releases are published via
[GitHub Releases](https://github.com/SpaceSquare640/GTAV_Mods_Manager/releases), each with a
SHA-256 checksum — verify your download against it. We take no responsibility for copies
obtained elsewhere.

## Workspace layout

- `crates/gtavmm-core` — all business logic: game/edition detection (Legacy/Enhanced, SP/LSPDFR,
  FiveM client/server), mod install/uninstall/enable/disable, conflict and protected-file
  checks, recycle bin, full backups, multi-profile switching, FiveM resource dependency
  resolution, an SP-add-on-vehicle-pack → FiveM converter, and an opt-in AI assistant module
  (local Ollama or provider-specific API-key integrations for Ollama Cloud, OpenAI, Anthropic,
  DeepSeek, OpenRouter, Google AI, and NVIDIA AI). Translation output is protected against
  overwriting source or existing files. No UI or CLI dependencies.
- `crates/gtavmm-cli` — command-line interface over `gtavmm-core`. Today this is the most
  complete way to use every feature; it will remain available as a power-user mode once the
  desktop app catches up.
- `crates/gtavmm-app` — the Tauri v2 + React + TypeScript desktop app. Scaffolded and under
  active development; see the Wiki for current page-by-page status.

## Development

Automated Rust and frontend validation runs on GitHub-hosted Windows and Ubuntu runners through
[GitHub Actions CI](.github/workflows/ci.yml). Release builds reuse the same validation workflow
before publishing installers. Local commands below remain available for interactive development.

```
cargo build
cargo test
cargo run -p gtavmm-cli -- --help
```

To run the desktop app in dev mode:

```
cd crates/gtavmm-app
npm install
npm run tauri dev
```

## Documentation

The [Wiki](https://github.com/SpaceSquare640/GTAV_Mods_Manager/wiki) has the full user-facing
documentation: getting started, installing mods, managing your library, the SP→FiveM converter,
backup/restore, the safety model, advanced tools, FAQ, and the changelog.

## Legal

- [Terms of Use](TERMS.md)
- [Privacy Policy](PRIVACY.md)
- [Contributing](CONTRIBUTING.md)

## Disclaimer

This is an unofficial, non-commercial, source-available project and is not affiliated with or
endorsed by Rockstar Games or Take-Two Interactive. See [TERMS.md](TERMS.md) for details.
