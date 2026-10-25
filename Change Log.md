# Change Log

<!-- OpenAI-Codex -->

## Unreleased

### Added

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
