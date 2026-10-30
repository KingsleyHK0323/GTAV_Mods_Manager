# GTAV Mods Manager

[Visit the GTAV Mods Manager website](https://spacesquare640.github.io/GTAV_Mods_Manager/)

Code: PolyForm Noncommercial License · Website content: CC BY-NC-SA

GTAV Mods Manager is a source-available, non-commercial desktop app for installing, translating and managing GTA V mods. It is not affiliated with Rockstar Games or Take-Two Interactive.

Current development focuses on GTA V Legacy single-player and Legacy LSPDFR. GTA V Enhanced and FiveM (client, server tools, and conversion) are under development and blocked by the backend. Existing settings, mod records, and pending-mode recycle-bin snapshots are retained. The UI will be updated separately. OIV packages remain unsupported; use OpenIV to install them.

Overlaps with files tracked by another active mod require explicit confirmation, including when the overlap resembles an update. Reinstall checks the new package and known conflicts before uninstalling the old version, but later I/O or database failures may still require restoring the old version from the recycle bin.

Existing target files without a recorded active owner also require confirmation. The conflict report identifies their owner as unknown. A reinstall only exempts its own restored original when its content matches the backup recorded before uninstall; changed files and other active owners still require confirmation. Non-regular targets, including symbolic links, are rejected.

Reinstall also requires every recorded old backup to remain available as a regular file, because uninstall processes the old version's complete file set. Reinstalling a disabled mod may require confirmation for original files already restored by disable.

Uninstall validates its targets and reads every recorded backup before changing files. Missing or unreadable backups stop the operation before snapshot creation; failures after these checks can still require recovery, so this is not a complete rollback guarantee.

Uninstall rejects targets outside the supplied game root, unsafe relative paths, and existing ancestors that resolve outside that root. It also refuses any target tracked by another active mod. Because overwrite-chain ordering is not recorded, this conservative protection can block uninstalling either overlapping mod; it does not provide automatic conflict-chain removal. Disabled-mod snapshot handling remains incomplete. These preflight checks do not eliminate filesystem races or later I/O failures.

Rollback also reads its backup before removing the current target, to detect existing read locks early. These checks do not prevent files from changing afterward and do not provide atomic replacement.

Download installers only from [GitHub Releases](https://github.com/SpaceSquare640/GTAV_Mods_Manager/releases) and verify the SHA-256 checksum file attached to each release. See [PRIVACY.md](PRIVACY.md) and [TERMS.md](TERMS.md).

— Kingsley

<!-- OpenAI-Codex -->
<!-- Anthropic-Claude_Code_Responsible: Source_Code -->
