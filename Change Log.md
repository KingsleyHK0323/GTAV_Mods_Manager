# Change Log

<!-- OpenAI-Codex_Responsible: Source_Code -->
<!-- Updated by OpenAI-Codex: added scheduled Dependabot direct-merge retries. -->
<!-- Updated by OpenAI-Codex: corrected Tauri v2 Ubuntu dependencies for CI and Release. -->

## Unreleased

### Added

- Added a GitHub Actions workflow that directly merges eligible same-repository Dependabot pull requests only after verifying the latest successful CI run and current head SHA. It checks after successful CI, processes successful check-suite events delivered by GitHub, and scans open pull requests every 30 minutes from the default branch to retry after approvals or other required checks. It disables pre-existing native Auto-merge requests, rejects pull requests that change workflow files using current and previous paths, and uses GitHub's standard merge API so repository protection and review rules still apply. Pull requests requiring a merge queue remain open.
- Added weekly Dependabot version-update checks for Cargo, npm, and GitHub Actions; tagged installer releases now include GitHub-generated release notes.
- Added bounded `.rar` mod archive extraction for unencrypted, single-volume RAR 1.5+ archives with UTF-8 or Unicode metadata names. RAR 1.3/1.4, legacy names that require an unspecified code page, symbolic links, redirected entries, unsafe paths, duplicate/colliding names, and archives beyond configured entry or output limits are rejected.
- Added FiveM Client mod details and lifecycle controls for editing metadata, enabling, disabling, reinstalling, and uninstalling mods.
- Added mode-aware uninstall path resolution. FiveM Client uninstall now requires an explicit existing client directory; calls that omit a page mode keep the existing GTA V path behavior.
- Added a FiveM Client recycle-bin preview and restore modal. Its list is filtered to FiveM Client mods; existing recycle-bin callers that omit a mode keep the all-mode list.
- Added local persistence for the FiveM Client folder. The page loads the saved folder when opened and lets users save a replacement.
- Added Legacy SP batch enable/disable controls using the existing per-mod lifecycle commands; failed items remain selected for retry.
- Added Legacy LSPDFR batch enable/disable controls using the same existing per-mod lifecycle commands.
- Added AI translation drafts for saved INI/XML files in Tools; the original is preserved and existing same-language drafts are rejected.
- Added provider-specific AI translation support for Ollama Local, Ollama Cloud, OpenAI, Anthropic, and DeepSeek. Each API-key provider uses a separate OS credential-store entry; the prior generic cloud configuration remains available for saved settings.
- Added OpenRouter, Google AI, and NVIDIA AI providers to the shared AI translation and diagnosis layer, each with a separate OS credential-store entry.
- Added cloud-data and usage-cost notices beside translation actions and in AI provider settings.
- Added regression coverage for provider aliases, explicit FiveM Client paths, App preview database-lock release, and full-backup edition isolation.

### Fixed

- Updated the Ubuntu CI and Release package lists to use Tauri v2's Ayatana AppIndicator development package.
- Replaced a manual INI separator character comparison with an array-based pattern to satisfy Clippy without changing key parsing behavior.
- Matched the Tauri DLL inspection command return type to the fingerprinted snapshot consumed by the App.
- Validated generated INI/XML translation drafts before writing; changed INI sections/keys, XML parse errors, and changed XML element/attribute/namespace structure are rejected without creating a draft.
- Validated source XML and UTF-8 encoding declarations before contacting an AI provider.
- Preserved custom XML entity references in escaped text events, validated them with quick-xml 0.36.2's lifetime-safe resolver API, and matched declaration metadata to its byte-based API.
- Bound DLL translation and patching to the inspected file's SHA-256 fingerprint, discard stale in-flight AI results when the file or language changes, and preserve the public core inspection type's struct-literal compatibility.
- Clarified that translation inputs are sent unchanged and are not covered by diagnosis path de-identification.
- Kept indented INI assignments and section headers structural when continuation semantics are ambiguous.
- Validated DLL translation language tags at the UI, Tauri, and core boundaries, and constrained default output paths to the source directory.
- Formatted the Rust source files reported by GitHub Actions so the workspace formatting check passes.
- Preserved explicit LSPDFR game-root layouts in mod folders and archives, including one `Grand Theft Auto V` wrapper, while keeping ordinary replacer files under `mods` and refusing ambiguous DLL packages.
- Marked App full backups with their GTA V edition, blocked unmarked or mismatched restores, and kept readable backups available when another archive is damaged.
- Made page-target resolution require a concrete App page so provider aliases cannot bypass edition checks.
- Routed App page lifecycle actions and page tools through the configured GTA path and rejected page/edition mismatches before acting.
- Saved the detected GTA install path when first-run onboarding is completed without a manual path selection.
- Kept onboarding path selection, detection, and completion from racing during validation; re-detection clears the previous manual selection so the displayed and saved path stay aligned.
- Added `.rar` to the reinstall file picker and removed unsupported `.oiv` packages from the install picker.
- Limited the Legacy SP App Excel export to Legacy SP installed and uninstalled records.
- Fixed the FiveM Client retry button passing a React mouse event to the optional boolean argument of the mod loader.
- Marked FiveM Server temporarily unavailable in App navigation and page routing while preserving its implementation for future re-enabling.
- Updated App GTA SP/LSPDFR previews and installs to require a configured path and reject page/edition mismatches instead of falling back to automatic detection.
- Recorded the target root and exact snapshotted file set for recycle-bin entries. Restore now uses that root, does not require GTA V root detection for entries with metadata, and preserves the entry when any recorded snapshot is missing.
- Added database schema migration v12 for recycle-bin snapshot metadata. Legacy entries continue to use the resolved game root and are retained if their required snapshots are incomplete.
- Added database schema migration v13 for the saved FiveM Client folder, separate from the GTA V install-path override.
- Added database schema migration v14 to allow RAR installs while preserving installed mods, profile links, file records, install history, and ID sequencing.
- Revalidated the FiveM Client folder before previewing or installing so a stale saved path cannot recreate a removed client directory.
- Created INI/XML translation drafts and patched DLL outputs with exclusive file creation, preventing races from overwriting existing drafts, output files, or source DLLs.
- Disabled the DLL translation UI option that previously allowed in-place overwrite of the original file.
