# Change Log

<!-- OpenAI-Codex -->
<!-- OpenAI-Codex_Responsible: Source_Code -->
<!-- Updated by OpenAI-Codex: added scheduled Dependabot direct-merge retries. -->
<!-- Updated by OpenAI-Codex: corrected Tauri v2 Ubuntu dependencies for CI and Release. -->

## Unreleased

### Added
- Prepare field-level general security comparison observations and a separate diagnostic-coverage report flag. Emit only presence/equality booleans and masked control values; preserve equality predicates, native query count, original failures and all pass gates. No production or dependency changes. This diagnostic revision has not been compiled or executed. <!-- OpenAI-Codex -->


- Prepare an in-memory component-consistency check between the existing general/SACL baselines and the kernel BACKUP result, with bounded component extraction, absent/null ACL distinctions and a separate report marker. No additional native security queries, production changes or raw security-data output. Original failures remain authoritative. The approved 8cc5f47 experiment compiled and observed general mismatch/SACL agreement, verified restoration and preserved the original BACKUP error 5; later cases stopped. <!-- OpenAI-Codex -->

- Prepare one additional same-handle BACKUP query through `GetKernelObjectSecurity`, using an initialized aligned 1 MiB caller-owned buffer, immediate failure capture and bounded self-relative descriptor validation without sizing, resizing or retries. Report its result independently; retain every original query failure and the full-security stop condition. No production, dependency or fixture-scope changes. The approved eade950 experiment compiled and observed kernel BACKUP success (valid self-relative descriptor, 140 bytes) versus GetSecurityInfo BACKUP error 5; privilege restoration was verified and subsequent cases stopped. <!-- OpenAI-Codex -->

- Prepare an ignored query-only diagnostic matrix using the same target handle, access mask and process-token state for general owner/group/DACL, SACL and BACKUP security queries. Record each status, descriptor validity and requested-part presence without raw descriptors or SIDs; distinguish absent and present-null ACLs. Continue only within the fixed three-query diagnostic matrix, then fail the case if any query failed, including BACKUP. Add it first in the manual workflow with explicit matrix report checks and no replacement-verification claim. Production code, dependencies, strict preservation cases and privilege restoration remain unchanged. The approved d84a28a run compiled the three-query matrix and observed general/SACL success, BACKUP error 5, verified privilege restoration and stopped later cases. <!-- OpenAI-Codex -->

- Add diagnostic-only callsite/API stages and before/after observations for full-security file opens and `GetSecurityInfo`, plus explicit fixture DACL, timestamp and stream-preparation checkpoints. The single approved `f1e0683` experiment compiled and verified privilege enable/restore on its error path, but stopped with access-denied error 5 before replacement; its shared error label cannot locate the failing API. Preserve all access masks, privilege behavior and strict security requirements. A separately approved 38aa69c run compiled and located error 5 in the first target GetSecurityInfo(BACKUP_SECURITY_INFORMATION), after both fixture opens succeeded and before metadata preparation or replacement; privilege restoration was verified. <!-- OpenAI-Codex -->

- Prepare an ignored comparison case that verifies an initially disabled security privilege is restored before replacement, then re-enables it only for full post-operation observations. Retain the original strict probes, require the new privilege-state observation in the manual report, and preserve the original operation error when restoration also fails. Update the manual sequence and synthetic fixture report scope; no production DLC, dependency or lockfile changes are included. The approved f1e0683 experiment compiled these sources but stopped at a full-security observation before replacement. <!-- OpenAI-Codex -->

- Prepare an explicitly opted-in, remote-test-only wrapper for an existing `SeSecurityPrivilege`, with bounded token queries, exact adjustment error checks, serialized access and explicit verified restoration on success, failure or panic. Add a separate manual native probe workflow with exact revision/approval gates, locked probe-only compilation, separate serial case processes and sanitized reports. Only the existing Windows Threading binding feature is enabled; production DLC writes and the two failing regression tests remain unchanged. The approved f1e0683 experiment verified privilege enable and restoration on its access-denied error path; replacement and complete ACL preservation remain unverified. <!-- OpenAI-Codex -->

- Prepare ignored Windows-only replacement metadata/sharing probes and a manual-only remote dependency resolver, without changing production DLC writes or ordinary CI. The probes require explicit hosted-Windows scratch approval and full security readability, retain recovery files, and report incomplete coverage. Remote CI for `afecba8` passed all-targets compilation and Clippy on Windows and Ubuntu; Ubuntu tests passed, while Windows retained the two existing DLC replacement failures. Adopt the independently reviewed `Cargo.lock` from remote resolver run 41185646022, adding only the core `windows-sys 0.61.2` dependency edge. Post-adoption CI for `fb306b9` also passed all-targets compilation and Clippy on both platforms; Ubuntu tests passed and the same two Windows DLC tests failed. Native replacement behavior remains unverified; the later approved experiment stopped before replacement. <!-- OpenAI-Codex -->

### Changed

- Update the resolver repository guard, update-check endpoint, issue-draft URL and regression expectation, package repository metadata, and repository documentation links for the move to `KingsleyHK0323/GTAV_Mods_Manager`. Preserve the existing Pages URL, author metadata, local Git remote, and workflow approval/SHA restrictions. <!-- OpenAI-Codex -->

- Marked FiveM client and GTA V Enhanced provider resolution as under development while focusing on Legacy; other mode entry points still require restrictions.
- Extended pending-mode restrictions to existing mod lifecycle actions, profile switching, recycle-bin restoration, full-backup operations, FiveM server tools and conversion, and CLI mode selection. Preserved stored data and pending-mode recycle-bin snapshots; UI changes are deferred.
- Corrected Rust formatting in provider tests and existing 7z path-validation code.

### Fixed

- Align the recorded-root legacy restore regression test with retained snapshot storage, checking restored content, the unchanged fallback root, file hash, active status, and entry removal without changing production cleanup behavior.
- Commit all recycle-bin restore file metadata, mod status, event, and entry removal in one database transaction. SQL failure preserves the previous metadata and recovery storage, while target files may already contain restored payloads; filesystem rollback remains separate.
- Compile the three legacy App uninstall/reinstall test wrappers only for unit tests, preventing production `dead_code` warnings from blocking Clippy while preserving the staging-aware commands and existing tests.
- Require independently supplied recycle, backup, and staging roots for filesystem snapshot cleanup. Reject unverifiable direct-child scope, linked/reparse trees, protected workspace/storage overlaps, and shared snapshot references. Route App and CLI callers through root-aware restore and expiry APIs; legacy restore retains snapshot files and legacy sweeping retains all entries. Revalidate after restore commit and log cleanup failure without reporting the completed restore as failed; concurrent replacement and full rollback remain unresolved.
- Preflight recycle-bin restore paths, source readability, target ancestry, duplicate targets, and overlapping storage before writes. Exclusively create independent backup candidates and complete all required backups before changing targets; retain partial backups on failure. Hard-link identity, race-free cleanup, and complete recovery remain pending.
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

### Semi-automatic DLC registration implementation (validation pending)

- Replace automatic DLC XML registration in the file lifecycle with an additive schema-v15 append-only intent/settlement ledger. Reinstallation shares one intent across old/new packs; failed operations remain reviewable. Display shared-root reminders in Legacy workspaces, CLI operations and AI action results, with independent lookup-error reporting. Add source-first tests for XML preservation, migration/reopening, active/disabled uninstall, restore, compound reinstall and database failures. The dedicated editor and AI XML suggestions remain a later phase. No compilation or runtime validation has been performed for these changes. <!-- OpenAI-Codex -->

- Retire `mod_analyzer::dlc::add_entry`, `add_entry_with_status`, `remove_entry`, and the obsolete missing-file installation error. Remove their nine old writer tests, including the two previously failing Windows replacement cases, while retaining pack discovery and four read-only inspection characterization cases. This is a product-contract retirement, not a repair of the old writer or evidence of passing CI. No tests have been executed. <!-- OpenAI-Codex -->
