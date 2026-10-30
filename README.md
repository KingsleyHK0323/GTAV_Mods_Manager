# GTAV Mods Manager

[Visit the GTAV Mods Manager website](https://spacesquare640.github.io/GTAV_Mods_Manager/)

Code: PolyForm Noncommercial License · Website content: CC BY-NC-SA

GTAV Mods Manager is a source-available, non-commercial desktop app for installing, translating and managing GTA V mods. It is not affiliated with Rockstar Games or Take-Two Interactive.

Current development focuses on GTA V Legacy single-player and Legacy LSPDFR. GTA V Enhanced and FiveM (client, server tools, and conversion) are under development and blocked by the backend. Existing settings, mod records, and pending-mode recycle-bin snapshots are retained. The UI will be updated separately. OIV packages remain unsupported; use OpenIV to install them.

Overlaps with files tracked by another active mod require explicit confirmation, including when the overlap resembles an update. Reinstall checks the new package and known conflicts before uninstalling the old version, but later I/O or database failures may still require restoring the old version from the recycle bin.

Existing target files without a recorded active owner also require confirmation. The conflict report identifies their owner as unknown. A reinstall only exempts its own restored original when its content matches the backup recorded before uninstall; changed files and other active owners still require confirmation. Non-regular targets, including symbolic links, are rejected.

Reinstall also requires every recorded old backup to remain available as a regular file, because uninstall processes the old version's complete file set. Reinstalling a disabled mod may require confirmation for original files already restored by disable.

Download installers only from [GitHub Releases](https://github.com/SpaceSquare640/GTAV_Mods_Manager/releases) and verify the SHA-256 checksum file attached to each release. See [PRIVACY.md](PRIVACY.md) and [TERMS.md](TERMS.md).

— Kingsley

<!-- OpenAI-Codex -->
<!-- Anthropic-Claude_Code_Responsible: Source_Code -->
