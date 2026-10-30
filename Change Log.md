# Change Log

<!-- OpenAI-Codex -->
<!-- OpenAI-Codex_Responsible: Source_Code -->
<!-- Updated by OpenAI-Codex: added scheduled Dependabot direct-merge retries. -->
<!-- Updated by OpenAI-Codex: corrected Tauri v2 Ubuntu dependencies for CI and Release. -->

## Unreleased

### Changed

- Marked FiveM client and GTA V Enhanced provider resolution as under development while focusing on Legacy; other mode entry points still require restrictions.
- Extended pending-mode restrictions to existing mod lifecycle actions, profile switching, recycle-bin restoration, full-backup operations, FiveM server tools and conversion, and CLI mode selection. Preserved stored data and pending-mode recycle-bin snapshots; UI changes are deferred.
- Corrected Rust formatting in provider tests and existing 7z path-validation code.

### Fixed

- Preflight recycle-bin restore paths, source readability, target ancestry, duplicate targets, and overlapping storage before writes. Exclusively create independent backup candidates and complete all required backups before changing targets; retain partial backups on failure. Hard-link identity, races, trusted snapshot cleanup, and complete recovery remain pending.
- Require recycle-bin restore owners to remain uninstalled and reject the entire restore before writes when any target is tracked by another active mod. Preserve the existing backup behavior for regular targets without an active owner.
- Snapshot disabled mods from verified staged payloads into the app-owned recycle bin without changing game targets, original backups, or shared DLC metadata. Preserve old core API signatures, require explicit staging context for disabled lifecycle operations, and connect App, CLI, and AI callers. Record staged file identities and actual hashes; conservatively validate legacy staging against recorded hashes. Retain staging and backups for recovery; DLC handling and complete transaction recovery remain pending.
- Reject uninstall targets outside the supplied game root or resolving through an escaping ancestor, and refuse active-uninstall targets tracked by another active mod before snapshotting. This conservative guard can block either overlapping mod; overwrite-chain removal remains incomplete.
- Read rollback backups before removing the current target to detect existing read locks early; this does not provide atomic replacement.
- Validate all uninstall targets and recorded backups before snapshotting or deleting files, rejecting missing, non-regular, unreadable, or self-referential backups.
- Require confirmation before overwriting untracked existing files, report unknown owners without fabricated IDs, and restrict reinstall exemptions to matching restored-original content.
- Require confirmation for all tracked file ownership conflicts, even when overlap suggests an update; validate a reinstall package and unrelated conflicts before removing the current version.
- Restored newly written and overwritten files when an install database transaction fails.
- Report filesystem rollback failures alongside the original install database error.
- Continue remaining rollback actions after failures, report each failure, and preserve existing DLC entries and files when their backups are missing.
- Removed ZIP and 7z extraction staging directories when inspection plans are dropped, and rejected archives over the entry and declared output-size limits.
- Rejected absolute, drive-prefixed, and parent-directory paths in 7z archive entries before extracting them.
- Restricted the text editor to existing, non-protected INI/XML/JSON/META/CFG/TXT files with absolute paths and a 5 MiB size limit; symbolic links are rejected.

## [1.0.0] - 2026-10-28 (prepared locally; not yet tagged or released)
<!-- Anthropic-Claude_Code_Responsible: Source_Code -->


### Added

- Added a manual GitHub latest-release lookup to mod details. It displays public release metadata only; it does not compare against an installed mod version or download/install a release.
- Added a GitHub Actions workflow that directly merges eligible same-repository Dependabot pull requests only after verifying the latest successful CI run and current head SHA. It checks after successful CI, processes successful check-suite events delivered by GitHub, and scans open pull requests every 30 minutes from the default branch to retry after approvals or other required checks. It disables pre-existing native Auto-merge requests, rejects pull requests that change workflow files using current and previous paths, and uses GitHub's standard merge API so repository protection and review rules still apply. Pull requests requiring a merge queue remain open.
- Added weekly Dependabot version-update checks for Cargo, npm, and GitHub Actions; tagged installer releases now include GitHub-generated release notes.
- Added per-platform SHA-256 checksum files for the Windows and Linux installers to the same GitHub Release draft, which is automatically published after both platforms finish successfully.
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

- Hardened tagged releases by validating stable tags against the Cargo, npm, and Tauri versions and separating read-only installer builds from release publication.
- Fixed the recycle-bin restore-root regression test fixture to create nested snapshot directories before writing the snapshot.
- Addressed workspace Clippy findings in the Tauri command layer without changing user-visible command behavior.
- Corrected the Enhanced LSPDFR provider note to use the public-preview announcement date (2025-12-26) instead of the stale 2026-04 reference.
- Corrected the Enhanced LSPDFR UI guidance to reflect the public-preview announcement date and the official Preview label checked on 2026-10-27.
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
