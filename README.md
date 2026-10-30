# GTAV Mods Manager

[Visit the GTAV Mods Manager website](https://spacesquare640.github.io/GTAV_Mods_Manager/)

Code: PolyForm Noncommercial License · Website content: CC BY-NC-SA

GTAV Mods Manager is a source-available, non-commercial desktop app for installing, translating and managing GTA V mods. It is not affiliated with Rockstar Games or Take-Two Interactive.

Current development focuses on GTA V Legacy single-player and Legacy LSPDFR. GTA V Enhanced and FiveM (client, server tools, and conversion) are under development and blocked by the backend. Existing settings, mod records, and pending-mode recycle-bin snapshots are retained. The UI will be updated separately. OIV packages remain unsupported; use OpenIV to install them.

Overlaps with files tracked by another active mod require explicit confirmation, including when the overlap resembles an update. Reinstall checks the new package and known conflicts before uninstalling the old version, but later I/O or database failures may still require restoring the old version from the recycle bin.

Existing target files without a recorded active owner also require confirmation. The conflict report identifies their owner as unknown. A reinstall only exempts its own restored original when its content matches the backup recorded before uninstall; changed files and other active owners still require confirmation. Non-regular targets, including symbolic links, are rejected.

Reinstalling an active mod also requires every recorded old backup to remain available as a regular file, because uninstall processes the old version's complete file set. Reinstalling a disabled mod may require confirmation for original files already restored by disable; it does not exempt them as newly restored backups.

Active uninstall validates its targets and reads every recorded backup before changing files. Missing or unreadable backups stop the operation before snapshot creation; failures after these checks can still require recovery, so this is not a complete rollback guarantee.

Active uninstall rejects targets outside the supplied game root, unsafe relative paths, and existing ancestors that resolve outside that root. It also refuses any target tracked by another active mod. Because overwrite-chain ordering is not recorded, this conservative protection can block uninstalling either overlapping mod; it does not provide automatic conflict-chain removal. These preflight checks do not eliminate filesystem races or later I/O failures.

The app uses its own recycle-bin snapshot directory, never the operating system's recycle bin. Disabled uninstall snapshots verified staged payloads without modifying current game files, original backups, or shared DLC metadata. New disable operations record file identities and actual staged hashes, including user edits; older staging without a manifest must match its recorded file hashes or the operation stops for recovery. Staging and old backups are retained, so this can temporarily use additional storage. Restore uses the existing snapshot format and reactivates the mod; DLC entry handling and full transactional recovery remain incomplete. Core callers must use the staging-aware uninstall/reinstall APIs for disabled mods; old APIs reject missing context. App, CLI, and AI commands supply their existing staging root.

Rollback also reads its backup before removing the current target, to detect existing read locks early. These checks do not prevent files from changing afterward and do not provide atomic replacement.

Recycle-bin restore requires the associated mod to remain uninstalled and checks the complete restore target set for other active owners before creating backups or changing files. An ownership conflict preserves the entry for later recovery. Existing regular files without an active owner keep the existing backup-and-replace behavior; the app cannot prove whether they are originals or newly added external files.

Restore preflights absolute paths, safe relative components, resolved source/target ancestry, ordinary readable files, duplicate targets, and overlapping snapshot/target/backup storage. Every required backup completes before the first target write. Backup candidates use exclusive creation and the same open handle for copying, preserving existing files and partial recovery data on failure. These checks do not establish hard-link identity, prevent later directory replacement, or provide atomic target writes or complete filesystem/database rollback.

Restore requires an existing game-root directory and absolute game, snapshot, and backup roots; missing target subdirectories inside the game root can still be created. A dangling link in a backup candidate causes a conservative refusal instead of automatic collision retry.

After all target copies complete, restore commits every file hash and backup path, the mod status, restore event, and recycle-bin entry removal in one database transaction. SQL failure rolls back these metadata changes and leaves snapshots and new backups available for recovery. Target files may already contain restored payloads; this is database consistency protection, not filesystem rollback. Cleanup runs only after a successful commit.

App and CLI cleanup now supply their recycle, backup, and staging roots independently of database snapshot paths. A cleanup entry must be a direct child of the dedicated recycle root, with no linked/reparse ancestors or tree members, resolved-path workspace/storage overlaps, or other snapshot references. Unknown, missing, pending, active, or disabled owners and unverifiable snapshot directories are retained by expiry sweeping. CLI sweeping runs before commands; the App requests it from its tools effect, which may run repeatedly. There is no background scheduler.

Legacy core restore APIs still restore files and remove the restored entry, but retain its filesystem snapshot because they lack cleanup context; legacy `sweep_expired` returns zero without removing data. Call `restore_with_recycle_root` and `sweep_expired_with_root` with explicit storage roots for guarded cleanup. Retained snapshots or partial cleanup remnants can consume additional space and may need separately authorized recovery/cleanup. A post-commit cleanup failure logs the retained path without turning a successful restore into failure. Tree scanning and final revalidation do not establish hard-link or mount identity or eliminate concurrent path replacement; this is not a handle-based, race-free deletion guarantee.

Download installers only from [GitHub Releases](https://github.com/SpaceSquare640/GTAV_Mods_Manager/releases) and verify the SHA-256 checksum file attached to each release. See [PRIVACY.md](PRIVACY.md) and [TERMS.md](TERMS.md).

— Kingsley

<!-- OpenAI-Codex -->
<!-- Anthropic-Claude_Code_Responsible: Source_Code -->
