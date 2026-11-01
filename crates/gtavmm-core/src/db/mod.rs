// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_dlc_deep_audit

//! SQLite connection management and schema migration. No cloud/network component —
//! this is a purely local file, per the project's offline-first, no-first-party-server
//! design decision.

pub mod models;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::CoreResult;

const SCHEMA_SQL: &str = include_str!("schema.sql");
const PROFILE_SCHEMA_SQL: &str = include_str!("profile_schema.sql");
const CURRENT_SCHEMA_VERSION: i32 = 15;

/// Resolves the default database file location under the OS-appropriate app-data
/// directory (via the `directories` crate), e.g.
/// `%APPDATA%/GTAVModsManager/gtavmm.sqlite3` on Windows or
/// `~/.local/share/gtavmm/gtavmm.sqlite3` on Linux.
pub fn default_db_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "SpaceSquare", "GTAVModsManager")
        .map(|dirs| dirs.data_dir().join("gtavmm.sqlite3"))
}

/// Opens (creating if necessary) the database at `path` and applies pending migrations.
pub fn open(path: &Path) -> CoreResult<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// Opens an in-memory database with the schema applied — used by tests and by any
/// future integration test that wants a fake game install without touching real
/// app-data on disk.
pub fn open_in_memory() -> CoreResult<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", true)?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// Runs each version-gated migration step in order, so both a brand-new database
/// (`user_version` 0) and an existing pre-profile-system database (`user_version` 1)
/// end up fully migrated. Every step is written to be safe to (re-)run via
/// `CREATE TABLE IF NOT EXISTS`, except the one genuine `ALTER TABLE`, which is
/// best-effort (ignored if the column already exists — happens on a fresh database,
/// since `profile_schema.sql`'s tables already exist by the time it runs there).
fn run_migrations(conn: &Connection) -> CoreResult<()> {
    let user_version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version < 1 {
        conn.execute_batch(SCHEMA_SQL)?;
    }
    if user_version < 2 {
        conn.execute_batch(PROFILE_SCHEMA_SQL)?;
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN active_profile_id INTEGER REFERENCES profile(id)",
            [],
        );
    }
    if user_version < 3 {
        // AI Assistant System (opt-in) settings. The API key itself is never stored
        // here — see `ai_assistant`, which keeps it in the OS-native credential store.
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN ai_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE user_settings ADD COLUMN ai_provider TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN ai_ollama_model TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN ai_cloud_endpoint TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN ai_cloud_model TEXT",
            [],
        );
    }
    if user_version < 4 {
        // AI Workflow / Prompt template library: the user's own reusable prompt text,
        // independent of the AI Assistant's Action Schema (see `ai_assistant` module
        // docs) — this is just a CRUD store, no automated execution involved.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS prompt_template (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL,
                content    TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );",
        )?;
    }
    if user_version < 5 {
        // Records the original mod package's source path at install time — needed
        // for `reinstall_mod` (AI Action Schema, see `ai_assistant::action_schema`)
        // to know what to reinstall from. Nullable: rows from before this migration,
        // and any install path that's since moved/deleted on disk, simply can't be
        // reinstalled from — that's a real, disclosed limitation, not hidden.
        let _ = conn.execute("ALTER TABLE installed_mod ADD COLUMN source_path TEXT", []);
    }
    if user_version < 6 {
        // Low-risk action auto-approve whitelist (design doc §3.3, v0.8+): a
        // comma-separated list of Action Schema action kinds (e.g.
        // "disable_mod,enable_mod") the user has opted out of per-instance approval
        // for. Empty/NULL means nothing is whitelisted — every action still needs
        // explicit approval by default, matching the design doc's "預設仍是逐次確認".
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN auto_approve_action_kinds TEXT",
            [],
        );
    }
    if user_version < 7 {
        // Standalone mod-link bookmarks (design request, 2026-08-30): a user's own
        // saved list of mod page URLs (e.g. gta5-mods.com) they want to come back to
        // later — deliberately independent of `installed_mod`, since a bookmark is
        // useful before a mod is ever installed (or after it's uninstalled).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS saved_mod_link (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL,
                url        TEXT NOT NULL,
                notes      TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );",
        )?;
    }
    if user_version < 8 {
        // Tags a saved link into a UI tab (see `saved_links` module doc) — `NULL` is
        // the user's own general bookmark list, unaffected by anything below.
        let _ = conn.execute("ALTER TABLE saved_mod_link ADD COLUMN category TEXT", []);

        // Seeds the built-in "模組 Setup 建議" tab (user-provided list, 2026-08-30) —
        // the small set of prerequisite/setup tools almost every Legacy SP install
        // needs before mods themselves go on. Only runs once, at the point a database
        // crosses this migration, so a user who deletes one of these afterward doesn't
        // get it silently reintroduced on the next app start.
        let mod_setup_links: &[(&str, &str, &str)] = &[
            (
                "Script Hook V",
                "http://www.dev-c.com/gtav/scripthookv/",
                "The core foundation almost every .asi script mod depends on for Legacy. Legacy-only — Enhanced needs RAGE Plugin Hook instead; the version must match your game build exactly or the game will fail to launch.",
            ),
            (
                "ScriptHookVDotNet",
                "https://github.com/scripthookvdotnet/scripthookvdotnet-nightly/releases",
                "Adds a .NET runtime layer on top of Script Hook V so C#/VB.NET .dll mods can run — required for any ScriptHookVDotNet script mod.",
            ),
            (
                "Menyoo 2.0",
                "https://www.gta5-mods.com/scripts/menyoo-2-0",
                "The well-known all-purpose trainer/spawner and map editor. Many other mods (vehicle spawn presets, map placements) are distributed as Menyoo .xml files, making this a foundational tool in the SP mod ecosystem.",
            ),
            (
                "Gameconfig for Legacy & Enhanced",
                "https://www.gta5-mods.com/misc/gta-5-gameconfig-300-cars",
                "Raises the internal object-pool limits (vehicles, peds, objects, etc.) to prevent crashes once you've installed a lot of add-on mods. Must match your game version to work correctly.",
            ),
            (
                "KRYST4LCLR's Gameconfig",
                "https://www.gta5-mods.com/misc/kryst4lclr-s-gameconfig-updated-regularly",
                "An alternative gameconfig that's updated more frequently — the community generally considers it more actively maintained and quicker to catch up after game updates. Pick one or the other, never both at once.",
            ),
            (
                "HeapAdjuster",
                "https://www.gta5-mods.com/tools/heapadjuster",
                "Raises the game's runtime memory heap limit, preventing crashes when many high-resolution textures/mods are loaded at once.",
            ),
            (
                "Packfile Limit Adjuster",
                "https://www.gta5-mods.com/tools/packfile-limit-adjuster",
                "Raises the limit on how many packfiles (RPFs) the game can load at once — the fix for the common \"packfile limit\" crash once you've installed enough add-on mods (especially vehicles/maps).",
            ),
        ];
        for (name, url, notes) in mod_setup_links {
            conn.execute(
                "INSERT INTO saved_mod_link (name, url, notes, category) VALUES (?1, ?2, ?3, 'mod_setup')",
                rusqlite::params![name, url, notes],
            )?;
        }
    }
    if user_version < 9 {
        // Seeds the built-in "LSPDFR Mods" tab (user-provided list, 2026-08-31, sourced
        // from lcpdfr.com — notes below were written after reading each mod's own page,
        // not guessed) — the small set of LSPDFR/EUP mods almost every police-roleplay
        // setup builds on. Same one-time-only seeding rule as the mod_setup_links above:
        // this only runs once, at the point a database crosses this migration.
        let lspdfr_links: &[(&str, &str, &str)] = &[
            (
                "LSPD First Response",
                "https://www.lcpdfr.com/downloads/gta5mods/g17media/7792-lspd-first-response/",
                "The core LSPDFR plugin that turns GTA V into a police roleplay game. Runs on RAGE Plugin Hook. Officially supported on Legacy Edition only — a separate Public Preview build exists for Enhanced Edition. Some antivirus software flags LSPDFR/RPH as a false positive due to its use of memory hooking.",
            ),
            (
                "Emergency uniforms pack - Law & Order",
                "https://www.lcpdfr.com/downloads/gta5mods/character/8151-emergency-uniforms-pack-law-order/",
                "The core Emergency Uniforms Pack (EUP) — an all-in-one player/ped clothing system with lore-friendly, California-inspired law-enforcement uniforms for male and female characters. Licensed CC BY-NC-SA 4.0. Pairs with EUP Serve & Rescue, EUP Menu, and the EUPFR configs below.",
            ),
            (
                "Emergency uniforms pack - Serve & Rescue",
                "https://www.lcpdfr.com/downloads/gta5mods/character/16256-emergency-uniforms-pack-serve-rescue/",
                "Companion pack to EUP Law & Order, adding non-law-enforcement agency outfits (fire, EMS, construction, security, etc). Requires the base EUP Law & Order pack to be installed first.",
            ),
            (
                "EUP Menu",
                "https://www.lcpdfr.com/downloads/gta5mods/scripts/13245-eup-menu/",
                "In-game RAGE Plugin Hook menu for browsing and applying EUP outfits to multiplayer peds, including a GTA Online-style character creator with save/share support. Requires RAGE Plugin Hook 0.37+, RAGENativeUI 1.8.1+, and EUP Law & Order 8.3+ (EUP Serve & Rescue is optional but recommended).",
            ),
            (
                "EUP Badges",
                "https://www.lcpdfr.com/downloads/gta5mods/misc/32225-eup-badges/",
                "Adds 15+ prop badges from EUP for LSPDFR's \"Flash Badge\" feature (0.4.2+). Packaged as an .oiv — install via OpenIV, then manually assign the badge model to each agency in your LSPDFR agency data files.",
            ),
            (
                "EUPFR - Ultimate Edition",
                "https://www.lcpdfr.com/downloads/gta5mods/datafile/32429-eupfr-ultimate-edition/",
                "LSPDFR agency data configs that let AI/backup peds wear EUP outfits, covering the full agency roster (RHPD, DPPD, LSPP, LSIA, BCSO, FIB). Install by dropping the \"custom\" folder into Grand Theft Auto V\\lspdfr\\data. Some agency variants expect specific third-party vehicle packs to match.",
            ),
            (
                "EUPFR Basic Configurations",
                "https://www.lcpdfr.com/downloads/gta5mods/datafile/22400-eupfr-basic-configurations/",
                "A lighter EUPFR config covering only the core agencies (LSPD, LSSD, SAHP, FIB, NOOSE, SASP, DOA, NYSP, SASPA, etc), without RHPD/DPPD/BCSO. Same install method as EUPFR Ultimate Edition, but relies only on the base EUP packs — no extra vehicle packs required.",
            ),
            (
                "Emergency Lighting System",
                "https://www.lcpdfr.com/downloads/gta5mods/scripts/13865-emergency-lighting-system/",
                "In-depth ELS lighting/siren control system (200+ patterns across 4 light groups) for emergency vehicles — the GTA V successor to the classic ELS-IV. Requires Script Hook V and vehicle models specifically built for ELS. Not compatible with multiplayer, and does not currently work on GTA V Enhanced Edition.",
            ),
        ];
        for (name, url, notes) in lspdfr_links {
            conn.execute(
                "INSERT INTO saved_mod_link (name, url, notes, category) VALUES (?1, ?2, ?3, 'lspdfr')",
                rusqlite::params![name, url, notes],
            )?;
        }
    }
    if user_version < 10 {
        // Settings the interface needs but had nowhere to keep.
        //
        // `theme` is the user's choice of palette, not the resolved one:
        // "system" means follow the OS, so the stored value stays meaningful
        // when the OS preference later changes. NULL means never chosen, which
        // the app treats as "system".
        let _ = conn.execute("ALTER TABLE user_settings ADD COLUMN theme TEXT", []);

        // Which version of the terms was accepted, rather than a yes/no flag.
        // A boolean could not tell "accepted the current terms" from "accepted
        // something we have since rewritten", so revising them would either
        // silently pass or force everyone to re-accept with no way to choose.
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN terms_accepted_version TEXT",
            [],
        );

        // Whether first-run setup has been completed. Separate from the terms:
        // someone can accept the terms and quit before choosing game paths, and
        // should land back on setup rather than on an empty workspace.
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN onboarding_completed INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // Where full backups are written, when the default app-data location
        // will not do — a 16 GB mods folder does not always belong on the
        // system drive.
        let _ = conn.execute(
            "ALTER TABLE user_settings ADD COLUMN backup_root_override TEXT",
            [],
        );
    }
    if user_version < 11 {
        // Which page a mod belongs to.
        //
        // Until now nothing recorded this, so every mod page ran the same
        // unfiltered query and showed the same list — Legacy SP and Legacy
        // LSPDFR were literally the same screen with different titles.
        //
        // The value is the page, not the provider: `legacy-sp` and
        // `enhanced-sp` both install through the SP provider but are separate
        // workspaces, so a column holding only `sp` could not tell them apart.
        let _ = conn.execute("ALTER TABLE installed_mod ADD COLUMN mode TEXT", []);

        // Whether the mode above was guessed rather than recorded at install
        // time. Existing rows predate the column, so they can only be inferred
        // — and a guess the interface cannot distinguish from a fact is a guess
        // the user has no reason to check.
        let _ = conn.execute(
            "ALTER TABLE installed_mod ADD COLUMN mode_inferred INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // LSPDFR category (callouts / EUP & peds / vehicle packs / other).
        // Added here rather than in its own version so the two columns that
        // classify a mod arrive together.
        let _ = conn.execute("ALTER TABLE installed_mod ADD COLUMN category TEXT", []);

        // Backfill, once, gated by the schema version like the seed rows above.
        //
        // Install paths carry the answer for the two cases that matter: LSPDFR
        // plugins live under `Plugins\LSPDFR\` and FiveM resources under
        // `resources\`. Separators are normalised first because a path stored
        // on Windows uses backslashes and one stored on Linux does not.
        //
        // Anything left over falls to Legacy SP rather than staying NULL. NULL
        // would have to be shown on every page to avoid hiding it, which is
        // exactly the duplication this version exists to remove. Every
        // backfilled row is marked inferred and can be corrected.
        const NORMALISED: &str = "lower(replace(install_path, char(92), '/'))";
        conn.execute(
            &format!(
                "UPDATE installed_mod SET mode = 'legacy-lspdfr', mode_inferred = 1
                  WHERE mode IS NULL AND {NORMALISED} LIKE '%plugins/lspdfr%'"
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "UPDATE installed_mod SET mode = 'fivem-client', mode_inferred = 1
                  WHERE mode IS NULL AND {NORMALISED} LIKE '%/resources/%'"
            ),
            [],
        )?;
        conn.execute(
            "UPDATE installed_mod SET mode = 'legacy-sp', mode_inferred = 1 WHERE mode IS NULL",
            [],
        )?;
    }
    if user_version < 12 {
        // Preserve the original root and exact file set for safe recycle-bin restores.
        let mut stmt = conn.prepare("PRAGMA table_info(recycle_bin_entry)")?;
        let column_names = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        if !column_names.iter().any(|name| name == "snapshot_metadata") {
            conn.execute(
                "ALTER TABLE recycle_bin_entry ADD COLUMN snapshot_metadata TEXT",
                [],
            )?;
        }
    }
    if user_version < 13 {
        // Remember the user's FiveM Client root separately from the GTA V override.
        let mut stmt = conn.prepare("PRAGMA table_info(user_settings)")?;
        let column_names = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        if !column_names.iter().any(|name| name == "fivem_client_path") {
            conn.execute(
                "ALTER TABLE user_settings ADD COLUMN fivem_client_path TEXT",
                [],
            )?;
        }
    }
    if user_version < 14 {
        // Add RAR to the source type constraint without dropping mod history or
        // rows in installed_mod_file, install_event, or profile_mod. SQLite cannot
        // edit a CHECK constraint in place, so rebuild the parent table with foreign
        // key checks temporarily disabled and restore its AUTOINCREMENT high-water
        // mark after copying the existing rows.
        let foreign_keys_enabled: bool =
            conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        if foreign_keys_enabled {
            conn.pragma_update(None, "foreign_keys", false)?;
        }

        let migration = conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TEMP TABLE _gtavmm_installed_mod_v13_sequence (seq INTEGER);
             INSERT INTO _gtavmm_installed_mod_v13_sequence (seq)
                 SELECT seq FROM sqlite_sequence WHERE name = 'installed_mod';
             CREATE TABLE installed_mod_v14 (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 name         TEXT NOT NULL,
                 source_type  TEXT NOT NULL CHECK (source_type IN ('oiv', 'zip', 'sevenzip', 'rar', 'folder', 'asi', 'dll', 'menyoo_xml')),
                 install_path TEXT NOT NULL,
                 installed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                 status       TEXT NOT NULL CHECK (status IN ('active', 'disabled', 'uninstalled')) DEFAULT 'active',
                 notes        TEXT,
                 link         TEXT,
                 source_path  TEXT,
                 mode         TEXT,
                 mode_inferred INTEGER NOT NULL DEFAULT 0,
                 category     TEXT
             );
             INSERT INTO installed_mod_v14 (
                 id, name, source_type, install_path, installed_at, status, notes, link,
                 source_path, mode, mode_inferred, category
             ) SELECT
                 id, name, source_type, install_path, installed_at, status, notes, link,
                 source_path, mode, mode_inferred, category
               FROM installed_mod;
             DROP TABLE installed_mod;
             ALTER TABLE installed_mod_v14 RENAME TO installed_mod;
             UPDATE sqlite_sequence
                SET seq = MAX(seq, COALESCE((
                    SELECT seq FROM _gtavmm_installed_mod_v13_sequence LIMIT 1
                ), 0))
              WHERE name = 'installed_mod';
             INSERT INTO sqlite_sequence (name, seq)
             SELECT 'installed_mod', seq
               FROM _gtavmm_installed_mod_v13_sequence
              WHERE NOT EXISTS (
                    SELECT 1 FROM sqlite_sequence WHERE name = 'installed_mod'
              );
             DROP TABLE _gtavmm_installed_mod_v13_sequence;
             COMMIT;",
        );
        if migration.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        let restore_foreign_keys = if foreign_keys_enabled {
            conn.pragma_update(None, "foreign_keys", true)
        } else {
            Ok(())
        };
        migration?;
        restore_foreign_keys?;
    }
    if user_version < 15 {
        // OpenAI-Codex_dlc_deep_audit: additive reminders never rewrite mod data.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS manual_dlc_event (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_id INTEGER NOT NULL CHECK (operation_id > 0),
                root_key TEXT NOT NULL,
                root_display TEXT NOT NULL,
                archive_scope TEXT NOT NULL CHECK (archive_scope = 'dlclist'),
                pack_key TEXT NOT NULL,
                pack_display TEXT NOT NULL,
                operation TEXT NOT NULL CHECK (operation IN ('install','uninstall','restore','reinstall')),
                phase TEXT NOT NULL CHECK (phase IN ('intent','settled')),
                installed_mod_id INTEGER,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                UNIQUE(operation_id, root_key, archive_scope, pack_key, phase)
            );
            CREATE INDEX IF NOT EXISTS idx_manual_dlc_scope
                ON manual_dlc_event(root_key, archive_scope, pack_key, id);
            CREATE TRIGGER IF NOT EXISTS manual_dlc_event_no_update
                BEFORE UPDATE ON manual_dlc_event BEGIN
                    SELECT RAISE(ABORT, 'manual DLC events are append-only');
                END;
            CREATE TRIGGER IF NOT EXISTS manual_dlc_event_no_delete
                BEFORE DELETE ON manual_dlc_event BEGIN
                    SELECT RAISE(ABORT, 'manual DLC events are append-only');
                END;",
        )?;
    }
    if user_version < CURRENT_SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_db_applies_schema_and_seeds_singleton_settings_row() {
        let conn = open_in_memory().expect("schema should apply cleanly");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "user_settings singleton row should be seeded");
    }

    // OpenAI-Codex: Source-first coverage; runtime verification is not yet authorized.
    #[test]
    fn manual_dlc_v15_migration_preserves_v14_records_and_reopens_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manual-dlc.sqlite3");
        {
            // Build only the existing-table subset exercised below, without the
            // new migration. This must not create the new ledger in advance.
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            conn.execute_batch(SCHEMA_SQL).unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status) \
                 VALUES ('Existing pack', 'folder', '/synthetic/game', 'disabled')",
                [],
            )
            .unwrap();
            let mod_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, '/synthetic/game/mods/update/x64/dlcpacks/Existing/dlc.rpf', 'original-hash')",
                [mod_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO install_event (installed_mod_id, event_type, success) \
                 VALUES (?1, 'disable', 1)",
                [mod_id],
            )
            .unwrap();
            conn.execute("UPDATE user_settings SET language = 'zh-TW'", [])
                .unwrap();
            // The v15 migration must be additive to these v14-compatible tables.
            conn.pragma_update(None, "user_version", 14).unwrap();
        }
        for attempt in 0..2 {
            let conn = open(&path).unwrap();
            let version: i32 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 15);
            let (name, status, hash, language): (String, String, String, String) = conn
                .query_row(
                    "SELECT m.name, m.status, f.file_hash, s.language \
                     FROM installed_mod m JOIN installed_mod_file f ON f.installed_mod_id = m.id \
                     CROSS JOIN user_settings s WHERE m.name = 'Existing pack'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (name.as_str(), status.as_str()),
                ("Existing pack", "disabled")
            );
            assert_eq!(hash, "original-hash");
            assert_eq!(language, "zh-TW");
            let history: i64 = conn
                .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
                .unwrap();
            assert_eq!(history, 1);
            let events: i64 = conn
                .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                events, attempt,
                "reopening must preserve existing reminders without inventing work"
            );
            if attempt == 0 {
                conn.execute("INSERT INTO manual_dlc_event(operation_id,root_key,root_display,archive_scope,pack_key,pack_display,operation,phase) VALUES(1,'/synthetic/game','/synthetic/game','dlclist','existing','Existing','install','intent')", []).unwrap();
            }
            assert!(conn
                .execute("UPDATE manual_dlc_event SET phase='settled'", [])
                .is_err());
            assert!(conn.execute("DELETE FROM manual_dlc_event", []).is_err());
        }
    }

    /// Inserts a mod row the way a pre-v11 database would have held it: no
    /// mode, because the column did not exist yet.
    fn insert_legacy_row(conn: &Connection, name: &str, install_path: &str) {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status)
             VALUES (?1, 'folder', ?2, 'active')",
            rusqlite::params![name, install_path],
        )
        .unwrap();
    }

    #[test]
    fn v11_infers_a_mode_for_rows_that_predate_the_column() {
        let conn = open_in_memory().unwrap();
        // Simulate the pre-v11 state: rows exist, and no mode was ever set.
        conn.execute("UPDATE installed_mod SET mode = NULL", [])
            .unwrap();
        insert_legacy_row(
            &conn,
            "Better Chases",
            r"C:\Games\GTAV\Plugins\LSPDFRc.dll",
        );
        insert_legacy_row(&conn, "Menyoo", r"C:\Games\GTAV\menyooStuff");
        insert_legacy_row(&conn, "Vehicle pack", "/srv/fxserver/resources/vehicles");
        conn.execute(
            "UPDATE installed_mod SET mode = NULL, mode_inferred = 0",
            [],
        )
        .unwrap();

        conn.pragma_update(None, "user_version", 10).unwrap();
        run_migrations(&conn).unwrap();

        let mode_of = |name: &str| -> (String, i64) {
            conn.query_row(
                "SELECT mode, mode_inferred FROM installed_mod WHERE name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(mode_of("Better Chases"), ("legacy-lspdfr".into(), 1));
        assert_eq!(mode_of("Vehicle pack"), ("fivem-client".into(), 1));
        // Nothing in the path identifies it, so it falls to the one mode that
        // is actually reachable rather than staying unclassified.
        assert_eq!(mode_of("Menyoo"), ("legacy-sp".into(), 1));
    }

    #[test]
    fn re_running_v11_does_not_overwrite_a_corrected_mode() {
        let conn = open_in_memory().unwrap();
        insert_legacy_row(&conn, "Menyoo", r"C:\Games\GTAV\menyooStuff");
        // The user corrected the guess: it is LSPDFR despite the path.
        conn.execute(
            "UPDATE installed_mod SET mode = 'legacy-lspdfr', mode_inferred = 0 WHERE name = 'Menyoo'",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let (mode, inferred): (String, i64) = conn
            .query_row(
                "SELECT mode, mode_inferred FROM installed_mod WHERE name = 'Menyoo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mode, "legacy-lspdfr", "a correction must survive migration");
        assert_eq!(inferred, 0);
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = open_in_memory().unwrap();
        // Re-running should not error (CREATE TABLE IF NOT EXISTS + version gate).
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn v14_allows_rar_and_preserves_mod_children_and_autoincrement_sequence() {
        let conn = open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute_batch(
            "CREATE TABLE installed_mod_v13 (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 name         TEXT NOT NULL,
                 source_type  TEXT NOT NULL CHECK (source_type IN ('oiv', 'zip', 'sevenzip', 'folder', 'asi', 'dll', 'menyoo_xml')),
                 install_path TEXT NOT NULL,
                 installed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                 status       TEXT NOT NULL CHECK (status IN ('active', 'disabled', 'uninstalled')) DEFAULT 'active',
                 notes        TEXT,
                 link         TEXT,
                 source_path  TEXT,
                 mode         TEXT,
                 mode_inferred INTEGER NOT NULL DEFAULT 0,
                 category     TEXT
             );
             INSERT INTO installed_mod_v13 (
                 id, name, source_type, install_path, installed_at, status, notes, link,
                 source_path, mode, mode_inferred, category
             ) SELECT
                 id, name, source_type, install_path, installed_at, status, notes, link,
                 source_path, mode, mode_inferred, category
               FROM installed_mod;
             DROP TABLE installed_mod;
             ALTER TABLE installed_mod_v13 RENAME TO installed_mod;
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO installed_mod (id, name, source_type, install_path, status, mode, mode_inferred)
             VALUES (41, 'Existing mod', 'zip', '/game', 'active', 'legacy-sp', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE sqlite_sequence SET seq = 99 WHERE name = 'installed_mod'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash)
             VALUES (41, '/game/mod.asi', 'hash')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO install_event (installed_mod_id, event_type, success)
             VALUES (41, 'install', 1)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO profile (id, name) VALUES (3, 'Default')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO profile_mod (profile_id, installed_mod_id) VALUES (3, 41)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 13).unwrap();

        run_migrations(&conn).unwrap();

        let existing: (String, String, Option<String>) = conn
            .query_row(
                "SELECT source_type, mode, category FROM installed_mod WHERE id = 41",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(existing, ("zip".into(), "legacy-sp".into(), None));
        let child_counts: (i64, i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = 41),
                    (SELECT COUNT(*) FROM install_event WHERE installed_mod_id = 41),
                    (SELECT COUNT(*) FROM profile_mod WHERE installed_mod_id = 41)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(child_counts, (1, 1, 1));

        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status)
             VALUES ('RAR mod', 'rar', '/game', 'active')",
            [],
        )
        .unwrap();
        let rar_id: i64 = conn.last_insert_rowid();
        assert!(
            rar_id >= 100,
            "migration must preserve the AUTOINCREMENT high-water mark"
        );

        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        let foreign_keys_enabled: bool = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert!(foreign_keys_enabled);
    }
}
