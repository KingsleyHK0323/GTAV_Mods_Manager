// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code
// OpenAI-Codex_lspdfr_role_app

//! Command implementations, factored as plain functions so they're unit-testable
//! without a running Tauri app — the `#[tauri::command]` wrappers below just marshal
//! `tauri::State`/error-to-string for the frontend.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

use gtavmm_core::db::models::{InstalledMod, ModStatus};

/// Reads `installed_mod` rows (any status), ordered by id (install order).
///
/// `mode` filters to a single page's mods. `None` means every mod regardless of
/// page — what the profile membership table and the global search want, and what
/// this function did unconditionally before schema v11 gave mods a page.
pub fn list_mods_impl(conn: &Connection, mode: Option<&str>) -> Result<Vec<InstalledMod>, String> {
    if let Some(mode) = mode {
        crate::page_mode::PageMode::parse(mode)?.ensure_available()?;
    }
    let mut stmt = conn
        .prepare(
            "SELECT id, name, source_type, install_path, installed_at, status, notes, link,              mode, mode_inferred, category              FROM installed_mod WHERE (?1 IS NULL OR mode = ?1) ORDER BY id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([mode], |row| {
            let status_str: String = row.get(5)?;
            let status = match status_str.as_str() {
                "active" => ModStatus::Active,
                "disabled" => ModStatus::Disabled,
                _ => ModStatus::Uninstalled,
            };
            Ok(InstalledMod {
                id: row.get(0)?,
                name: row.get(1)?,
                source_type: row.get(2)?,
                install_path: row.get(3)?,
                installed_at: row.get(4)?,
                status,
                notes: row.get(6)?,
                link: row.get(7)?,
                mode: row.get(8)?,
                mode_inferred: row.get::<_, i64>(9)? != 0,
                category: row.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_mods(
    state: tauri::State<crate::AppState>,
    mode: Option<String>,
) -> Result<Vec<InstalledMod>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    list_mods_impl(&conn, mode.as_deref())
}

/// Records which page a mod belongs to, clearing the "inferred" flag.
///
/// Setting it by hand is the correction path for a guessed value, so the guess
/// marker must come off — otherwise the interface would keep asking the user to
/// check something they have already checked.
pub fn set_mod_mode_impl(conn: &Connection, mod_id: i64, mode: &str) -> Result<(), String> {
    let page = crate::page_mode::PageMode::parse(mode)?;
    page.ensure_available()?;
    gtavmm_core::providers::ensure_installed_mod_available(conn, mod_id)
        .map_err(|error| error.to_string())?;
    let changed = conn
        .execute(
            "UPDATE installed_mod SET mode = ?1, mode_inferred = 0 WHERE id = ?2",
            rusqlite::params![page.as_str(), mod_id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("no installed mod with id {mod_id}"));
    }
    Ok(())
}

#[tauri::command]
pub fn set_mod_mode(
    state: tauri::State<crate::AppState>,
    mod_id: i64,
    mode: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    set_mod_mode_impl(&conn, mod_id, &mode)
}

/// Serializable summary of a `game_locator::detect()` outcome — `DetectResult`
/// itself isn't `Serialize`, and the frontend doesn't need the full enum shape.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DetectGameResult {
    Found {
        install_path: String,
        edition: String,
    },
    NotFound,
}

pub fn detect_game_impl() -> Result<DetectGameResult, String> {
    use gtavmm_core::game_locator::DetectResult;
    match gtavmm_core::game_locator::detect() {
        Ok(DetectResult::Found(installation)) => Ok(DetectGameResult::Found {
            install_path: installation.install_path,
            edition: installation.edition,
        }),
        Ok(DetectResult::FoundUnsupportedEdition { .. }) | Ok(DetectResult::NotFound) => {
            Ok(DetectGameResult::NotFound)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn detect_game() -> Result<DetectGameResult, String> {
    detect_game_impl()
}

pub fn fivem_resolve_load_order_impl(resources_root: &str) -> Result<Vec<String>, String> {
    gtavmm_core::fivem::resolve_load_order(std::path::Path::new(resources_root))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fivem_resolve_load_order(resources_root: String) -> Result<Vec<String>, String> {
    fivem_resolve_load_order_impl(&resources_root)
}

pub fn fivem_apply_load_order_impl(
    resources_root: &str,
    server_cfg_path: &str,
) -> Result<Vec<String>, String> {
    let result = gtavmm_core::fivem::apply_load_order(
        std::path::Path::new(resources_root),
        std::path::Path::new(server_cfg_path),
    )
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!(
            "fivem_apply_load_order failed for '{resources_root}' -> '{server_cfg_path}': {reason}"
        ));
    }
    result
}

#[tauri::command]
pub fn fivem_apply_load_order(
    resources_root: String,
    server_cfg_path: String,
) -> Result<Vec<String>, String> {
    fivem_apply_load_order_impl(&resources_root, &server_cfg_path)
}

pub fn convert_vehicle_pack_impl(
    dlc_rpf: &str,
    output_dir: &str,
) -> Result<gtavmm_core::sp_to_fivem::ConversionReport, String> {
    let result = gtavmm_core::sp_to_fivem::convert_vehicle_pack(
        std::path::Path::new(dlc_rpf),
        std::path::Path::new(output_dir),
    )
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!(
            "convert_vehicle_pack failed for '{dlc_rpf}' -> '{output_dir}': {reason}"
        ));
    }
    result
}

#[tauri::command]
pub fn convert_vehicle_pack(
    dlc_rpf: String,
    output_dir: String,
) -> Result<gtavmm_core::sp_to_fivem::ConversionReport, String> {
    convert_vehicle_pack_impl(&dlc_rpf, &output_dir)
}

/// Resolves a provider from either spelling the frontend may send.
///
/// A page name (`legacy-sp`, `enhanced-lspdfr`, …) is what the workspaces now
/// pass, since they also need the page recorded. The three bare provider names
/// stay accepted because the converter and the read-only inspect path have no
/// page to speak of and never had one.
fn mode_from_str(mode: &str) -> Result<gtavmm_core::providers::Mode, String> {
    if matches!(mode, "enhanced-sp" | "enhanced-lspdfr" | "fivem-client") {
        gtavmm_core::providers::ensure_mode_available(mode).map_err(|error| error.to_string())?;
    }
    match mode {
        "sp" => Ok(gtavmm_core::providers::Mode::Sp),
        "lspdfr" => Ok(gtavmm_core::providers::Mode::Lspdfr),
        "fivem-client" => Ok(gtavmm_core::providers::Mode::FivemClient),
        other => crate::page_mode::PageMode::parse(other)
            .map(crate::page_mode::PageMode::provider_mode)
            .map_err(|_| {
                format!("unknown mode: {other} (expected a page name or sp/lspdfr/fivem-client)")
            }),
    }
}

/// OpenAI-Codex: Normalize FiveM roots and reject missing folders before file operations.
fn normalize_game_path_for_mode(
    game_path: Option<&str>,
    mode: gtavmm_core::providers::Mode,
) -> Result<Option<&str>, String> {
    if mode != gtavmm_core::providers::Mode::FivemClient {
        return Ok(game_path);
    }

    let path = game_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "fivem-client requires an explicit client directory".to_string())?;
    if !std::path::Path::new(path).is_dir() {
        return Err(format!("{path} is not an existing FiveM client directory"));
    }
    Ok(Some(path))
}

/// OpenAI-Codex: Use a caller path first; GTA modes require a saved path otherwise.
fn effective_game_path_impl(
    conn: &Connection,
    game_path: Option<&str>,
    mode: gtavmm_core::providers::Mode,
) -> Result<Option<String>, String> {
    if let Some(path) = game_path.map(str::trim).filter(|path| !path.is_empty()) {
        return Ok(Some(path.to_string()));
    }
    if mode == gtavmm_core::providers::Mode::FivemClient {
        return Ok(None);
    }
    let settings = load_user_settings_impl(conn)?;
    match settings.game_install_path_override {
        Some(path) if !path.trim().is_empty() => Ok(Some(path.trim().to_string())),
        _ => Err(
            "Choose a GTA V installation path in Settings before previewing or installing."
                .to_string(),
        ),
    }
}

/// OpenAI-Codex: Reject a valid GTA root when it does not match the active page.
fn ensure_page_game_edition(
    page: crate::page_mode::PageMode,
    game_root: &std::path::Path,
) -> Result<(), String> {
    page.ensure_available()?;
    let Some(expected) = page.expected_game_edition() else {
        return Ok(());
    };
    let (_, actual) = gtavmm_core::providers::resolve_game_root(Some(game_root))
        .map_err(|error| error.to_string())?;
    if actual != expected {
        return Err(format!(
            "{} requires a {} GTA V install, but the selected game path resolves to {}: {}. Update the saved path in Settings.",
            page.as_str(),
            expected,
            actual,
            game_root.display()
        ));
    }
    Ok(())
}

/// OpenAI-Codex: Resolve App page operations from the selected install and guard its edition.
fn resolve_page_target(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
) -> Result<
    (
        std::path::PathBuf,
        Box<dyn gtavmm_core::providers::ModeProvider>,
    ),
    String,
> {
    let page = crate::page_mode::PageMode::parse(mode)?;
    page.ensure_available()?;
    let core_mode = page.provider_mode();
    let effective_game_path = effective_game_path_impl(conn, game_path, core_mode)?;
    let game_path = normalize_game_path_for_mode(effective_game_path.as_deref(), core_mode)?;
    let (game_root, provider) =
        gtavmm_core::providers::resolve(game_path.map(std::path::Path::new), core_mode)
            .map_err(|error| error.to_string())?;
    ensure_page_game_edition(page, &game_root)?;
    Ok((game_root, provider))
}

// OpenAI-Codex_atomic_review: Resolve the current Legacy workspace, then include both Legacy modes.
pub fn list_manual_dlc_tasks_impl(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
) -> Result<Vec<gtavmm_core::dlc_manual::ManualDlcTask>, String> {
    if !matches!(mode, "legacy-sp" | "legacy-lspdfr") {
        return Err(
            "Manual DLC tasks are available only for Legacy SP and Legacy LSPDFR.".to_string(),
        );
    }
    let (root, _) = resolve_page_target(conn, game_path, mode)?;
    gtavmm_core::dlc_manual::list_tasks(conn, Some(&root)).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_manual_dlc_tasks(
    state: tauri::State<'_, crate::AppState>,
    game_path: Option<String>,
    mode: String,
) -> Result<Vec<gtavmm_core::dlc_manual::ManualDlcTask>, String> {
    let conn = state.conn.lock().map_err(|error| error.to_string())?;
    list_manual_dlc_tasks_impl(&conn, game_path.as_deref(), &mode)
}

/// Read-only preview of what installing `path` would do — no files written, nothing
/// recorded in the database. Mirrors the CLI's `inspect` command.
pub fn inspect_mod_impl(
    game_path: Option<&str>,
    mode: &str,
    path: &str,
) -> Result<gtavmm_core::mod_analyzer::ModPlan, String> {
    let core_mode = mode_from_str(mode)?;
    let game_path = normalize_game_path_for_mode(game_path, core_mode)?;
    let (game_root, provider) =
        gtavmm_core::providers::resolve(game_path.map(std::path::Path::new), core_mode)
            .map_err(|e| e.to_string())?;
    if let Ok(page) = crate::page_mode::PageMode::parse(mode) {
        ensure_page_game_edition(page, &game_root)?;
    }
    gtavmm_core::mod_analyzer::classify(std::path::Path::new(path), provider.as_ref())
        .map_err(|e| e.to_string())
}

/// OpenAI-Codex: Resolve the saved GTA path for the app's read-only install preview.
#[cfg(test)]
pub fn inspect_mod_for_settings_impl(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
    path: &str,
) -> Result<gtavmm_core::mod_analyzer::ModPlan, String> {
    let core_mode = mode_from_str(mode)?;
    let effective_game_path = effective_game_path_impl(conn, game_path, core_mode)?;
    inspect_mod_impl(effective_game_path.as_deref(), mode, path)
}

/// OpenAI-Codex: Resolve an inspect path under the DB lock, then return it unlocked.
fn effective_game_path_for_inspect(
    conn: &std::sync::Mutex<Connection>,
    game_path: Option<&str>,
    mode: &str,
) -> Result<Option<String>, String> {
    let conn = conn.lock().map_err(|e| e.to_string())?;
    let core_mode = mode_from_str(mode)?;
    effective_game_path_impl(&conn, game_path, core_mode)
}

#[tauri::command]
pub fn inspect_mod(
    state: tauri::State<crate::AppState>,
    game_path: Option<String>,
    mode: String,
    path: String,
) -> Result<gtavmm_core::mod_analyzer::ModPlan, String> {
    let effective_game_path =
        effective_game_path_for_inspect(&state.conn, game_path.as_deref(), &mode)?;
    inspect_mod_impl(effective_game_path.as_deref(), &mode, &path)
}

/// OpenAI-Codex_install_preflight_backend: Current targets and ownership, never an install authorization.
#[derive(Debug, Serialize)]
pub struct InstallPreview {
    pub mode: String,
    pub game_root: String,
    pub plan: gtavmm_core::mod_analyzer::ModPlan,
    pub conflicts: gtavmm_core::conflict::ConflictReport,
    pub role: Option<crate::install_role::ManagedDllRole>,
    pub role_required: bool,
}

// OpenAI-Codex_install_preflight_backend: Only canonicalize the trusted resolved root.
// The client echoes this identity; its value is never used as an installation path.
fn preview_root_identity(root: &std::path::Path) -> Result<String, String> {
    let canonical = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    let identity = canonical
        .to_str()
        .ok_or_else(|| "The game root is not valid Unicode.".to_string())?;
    let identity = identity.to_string();
    Ok(identity)
}

fn ensure_install_preview_mode(mode: &str) -> Result<(), String> {
    let page = crate::page_mode::PageMode::parse(mode)?;
    page.ensure_available()?;
    if !matches!(mode, "legacy-sp" | "legacy-lspdfr") {
        return Err("Installation preview supports only Legacy SP and Legacy LSPDFR.".to_string());
    }
    Ok(())
}

// OpenAI-Codex_install_preflight_backend: Preserve the resolver's target spelling,
// including for missing files whose ownership keys cannot be canonicalized yet.
#[cfg(test)]
fn classify_install_preview(
    root: std::path::PathBuf,
    mode: &str,
    path: &std::path::Path,
) -> Result<gtavmm_core::mod_analyzer::ModPlan, String> {
    classify_install_preview_with_role(root, mode, path, None).map(|result| result.0)
}

fn classify_install_preview_with_role(
    root: std::path::PathBuf,
    mode: &str,
    path: &std::path::Path,
    role: Option<&str>,
) -> Result<
    (
        gtavmm_core::mod_analyzer::ModPlan,
        Option<crate::install_role::ManagedDllRole>,
        bool,
    ),
    String,
> {
    ensure_install_preview_mode(mode)?;
    // ModeProvider is not Send; construct the provider inside the worker.
    let provider: Box<dyn gtavmm_core::providers::ModeProvider> = match mode {
        "legacy-sp" => Box::new(gtavmm_core::providers::LegacySpProvider::new(root)),
        "legacy-lspdfr" => Box::new(gtavmm_core::providers::LegacyLspdfrProvider::new(root)),
        _ => {
            return Err(
                "Installation preview supports only Legacy SP and Legacy LSPDFR.".to_string(),
            )
        }
    };
    crate::install_role::classify(path, mode, provider.as_ref(), role)
}

#[cfg(test)]
fn finish_install_preview(
    conn: &Connection,
    mode: &str,
    root: &std::path::Path,
    plan: gtavmm_core::mod_analyzer::ModPlan,
) -> Result<InstallPreview, String> {
    finish_install_preview_with_role(conn, mode, root, plan, None, false)
}

fn finish_install_preview_with_role(
    conn: &Connection,
    mode: &str,
    root: &std::path::Path,
    plan: gtavmm_core::mod_analyzer::ModPlan,
    role: Option<crate::install_role::ManagedDllRole>,
    role_required: bool,
) -> Result<InstallPreview, String> {
    if let gtavmm_core::mod_analyzer::ModFormat::Unsupported(reason) = &plan.format {
        return Err(format!("Unsupported mod package: {reason}"));
    }
    if plan.files.is_empty() {
        return Err("The package contains no installable files.".to_string());
    }
    let targets = plan
        .files
        .iter()
        .map(|file| file.target.clone())
        .collect::<Vec<_>>();
    // Keep the plan (including its archive staging guard) alive through analysis.
    // All active owners matter, including owners recorded on a different page.
    let conflicts =
        gtavmm_core::conflict::analyze(conn, &targets).map_err(|error| error.to_string())?;
    Ok(InstallPreview {
        mode: mode.to_string(),
        game_root: preview_root_identity(root)?,
        plan,
        conflicts,
        role,
        role_required,
    })
}

/// OpenAI-Codex_install_preflight_backend: Classification may stage archives; no game/DB writes.
#[tauri::command]
pub async fn inspect_install_preview(
    state: tauri::State<'_, crate::AppState>,
    game_path: Option<String>,
    mode: String,
    path: String,
    role: Option<String>,
) -> Result<InstallPreview, String> {
    ensure_install_preview_mode(&mode)?;
    let root = {
        let conn = state.conn.lock().map_err(|error| error.to_string())?;
        // Resolve and reject pending pages before inspecting the package path.
        resolve_page_target(&conn, game_path.as_deref(), &mode)?.0
    };
    let worker_root = root.clone();
    let worker_mode = mode.clone();
    let (plan, role, role_required) = tauri::async_runtime::spawn_blocking(move || {
        classify_install_preview_with_role(
            worker_root,
            &worker_mode,
            std::path::Path::new(&path),
            role.as_deref(),
        )
    })
    .await
    .map_err(|_| "Installation preview could not complete.".to_string())??;
    let conn = state.conn.lock().map_err(|error| error.to_string())?;
    finish_install_preview_with_role(&conn, &mode, &root, plan, role, role_required)
}

fn check_install_preview_root(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
    expected_game_root: Option<&str>,
) -> Result<(), String> {
    if let Some(expected) = expected_game_root {
        let (root, _) = resolve_page_target(conn, game_path, mode)?;
        if preview_root_identity(&root)? != expected {
            return Err(
                "The game installation changed. Review the installation preview again.".to_string(),
            );
        }
    }
    Ok(())
}

/// Installs `path` for real. Mirrors the CLI's `install` command: classify, then run
/// the full install pipeline (conflict check, backup, write, record). `backup_root` is
/// a parameter (not computed internally) so this stays unit-testable against a temp
/// directory — the `#[tauri::command]` wrapper below points it at the real app-data
/// directory.
pub fn install_mod_impl(
    conn: &mut Connection,
    game_path: Option<&str>,
    mode: &str,
    path: &str,
    name: Option<&str>,
    override_foreign_conflicts: bool,
    backup_root: &std::path::Path,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    install_mod_with_role_impl(
        conn,
        game_path,
        mode,
        path,
        name,
        override_foreign_conflicts,
        backup_root,
        None,
    )
}

// OpenAI-Codex_lspdfr_role_app: Legacy callers retain the provider default by omitting a role.
#[allow(clippy::too_many_arguments)]
pub fn install_mod_with_role_impl(
    conn: &mut Connection,
    game_path: Option<&str>,
    mode: &str,
    path: &str,
    name: Option<&str>,
    override_foreign_conflicts: bool,
    backup_root: &std::path::Path,
    role: Option<&str>,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    install_mod_with_role_and_backup_impl(
        conn, game_path, mode, path, name, override_foreign_conflicts, backup_root, role, true,
    )
}

// OpenAI-Codex_backup_flow: Only the backend wrapper supplies a confirmed backup choice.
#[allow(clippy::too_many_arguments)]
fn install_mod_with_role_and_backup_impl(
    conn: &mut Connection,
    game_path: Option<&str>,
    mode: &str,
    path: &str,
    name: Option<&str>,
    override_foreign_conflicts: bool,
    backup_root: &std::path::Path,
    role: Option<&str>,
    auto_backup: bool,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    // The caller names a page, not a provider. Both are needed: the provider
    // decides where files go, and the page is what gets recorded so this mod
    // shows up on the workspace it was installed from and nowhere else.
    let page = crate::page_mode::PageMode::parse(mode)?;
    let (game_root, provider) = resolve_page_target(conn, game_path, mode)?;
    let input_path = std::path::Path::new(path);
    let (plan, _, _) = crate::install_role::classify(input_path, mode, provider.as_ref(), role)?;
    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    if plan.files.is_empty()
        && !matches!(
            &plan.format,
            gtavmm_core::mod_analyzer::ModFormat::Unsupported(_)
        )
    {
        gtavmm_core::providers::ensure_game_root_available(&game_root)
            .map_err(|error| error.to_string())?;
        return Err(
            gtavmm_core::error::CoreError::UnsupportedFormat {
                reason: "the install plan contains no files".to_string(),
            }
            .to_string(),
        );
    }
    let name = name.map(str::to_string).unwrap_or_else(|| {
        input_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed mod".to_string())
    });

    let options = gtavmm_core::install::InstallOptions {
        auto_backup,
        override_foreign_conflicts,
    };
    // OpenAI-Codex_Source_Code: Pass page metadata to the install transaction.
    // OpenAI-Codex_ui_report_review: Validate metadata before the install pipeline writes targets.
    // Stamp the page onto the new row. Recorded rather than inferred, so the
    // interface does not mark it as a guess.
    // The category is only meaningful on the LSPDFR pages, so it is left
    // NULL elsewhere rather than filed under a taxonomy that does not apply.
    let category = match page {
        crate::page_mode::PageMode::LegacyLspdfr
        | crate::page_mode::PageMode::EnhancedLspdfr => {
            Some(crate::lspdfr_category::infer(&plan))
        }
        _ => None,
    };
    let metadata = gtavmm_core::install::PageInstallMetadata::new(page.as_str(), category)
        .ok_or_else(|| "Page metadata is not supported.".to_string())?;
    let result = gtavmm_core::install::install_with_page_metadata(
        conn, &name, &plan, &game_root, backup_root, options, input_path, metadata,
    );
    // OpenAI-Codex_v1_removal_review: A managed-owner refusal is an expected conflict.
    // OpenAI-Codex_log_isolation_review: Avoid app log writes for identified database failures.
    match &result {
        Err(gtavmm_core::CoreError::ForeignConflict { .. })
        | Err(gtavmm_core::CoreError::Database(_))
        | Ok(_) => {}
        Err(gtavmm_core::CoreError::InstallDatabaseRollbackFailed { database_error, .. })
            if matches!(
                database_error.as_ref().downcast_ref::<gtavmm_core::CoreError>(),
                Some(gtavmm_core::CoreError::Database(_))
            ) => {}
        Err(reason) => {
            let _ =
                gtavmm_core::app_log::error(&format!("install_mod failed for '{name}': {reason}"));
        }
    }
    result.map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum BackupCommandError {
    Failure(String),
    Cancelled { code: &'static str },
}

impl From<String> for BackupCommandError {
    fn from(error: String) -> Self {
        Self::Failure(error)
    }
}

fn backup_cancelled() -> BackupCommandError {
    BackupCommandError::Cancelled {
        code: "backup_confirmation_cancelled",
    }
}

// OpenAI-Codex_backup_flow: Compare settings and same-connection writes after confirmation.
#[derive(Debug, PartialEq, Eq)]
struct BackupConfirmSnapshot {
    settings: gtavmm_core::db::models::UserSettings,
    changes: i64,
}

fn backup_confirm_snapshot(conn: &Connection) -> Result<BackupConfirmSnapshot, String> {
    Ok(BackupConfirmSnapshot {
        settings: load_user_settings_impl(conn)?,
        changes: conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .map_err(|error| error.to_string())?,
    })
}

fn recheck_backup_snapshot(
    conn: &Connection,
    expected: &BackupConfirmSnapshot,
) -> Result<(), String> {
    if &backup_confirm_snapshot(conn)? != expected {
        return Err("Settings or application data changed. Review and retry.".to_string());
    }
    Ok(())
}

fn checked_backup_choice(
    conn: &Connection,
    expected: &BackupConfirmSnapshot,
    confirmed: bool,
) -> Result<bool, BackupCommandError> {
    if !expected.settings.default_auto_backup && !confirmed {
        return Err(backup_cancelled());
    }
    recheck_backup_snapshot(conn, expected)?;
    Ok(expected.settings.default_auto_backup)
}

// OpenAI-Codex_backup_flow: A single native warning stays reserved until its callback is dropped.
static BACKUP_WARNING_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct BackupWarningGuard(&'static std::sync::atomic::AtomicBool);

impl Drop for BackupWarningGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn acquire_backup_warning(
    active: &'static std::sync::atomic::AtomicBool,
) -> Option<std::sync::Arc<BackupWarningGuard>> {
    active
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .ok()
        .map(|_| std::sync::Arc::new(BackupWarningGuard(active)))
}

fn backup_warning_callback(
    sender: std::sync::mpsc::Sender<bool>,
    guard: std::sync::Arc<BackupWarningGuard>,
) -> impl FnOnce(bool) + Send + 'static {
    move |confirmed| {
        let _guard = guard;
        let _ = sender.send(confirmed);
    }
}

#[derive(Clone, Copy)]
enum BackupWarning<'a> {
    Settings,
    Install { path: &'a str, mode: &'a str },
    Reinstall { id: i64, name: &'a str },
}

#[derive(Clone, Copy)]
enum BackupWarningFailure {
    Busy,
    TimedOut,
    Disconnected,
    Worker,
}

fn backup_warning_failure(language: &str, failure: BackupWarningFailure) -> String {
    let zh = language.eq_ignore_ascii_case("zh-TW");
    match (zh, failure) {
        (true, BackupWarningFailure::Busy) => "另一個備份確認仍未結束，無法開啟新確認。請嘗試回覆或關閉舊視窗；已逾時的回覆不會執行原操作。若仍無法解除，請重新啟動 App。",
        (true, BackupWarningFailure::TimedOut) => "備份確認已超過 120 秒，本次操作已停止，舊視窗之後的回覆無效。若舊視窗仍顯示，請嘗試回覆或關閉；確認未釋放前，後續確認會被拒絕。若無法解除，請重新啟動 App。",
        (true, BackupWarningFailure::Disconnected) => "備份確認沒有回傳結果，本次操作已停止。若舊視窗仍顯示，請嘗試回覆或關閉；已逾時的回覆無效。若後續確認仍被拒絕，請重新啟動 App。",
        (true, BackupWarningFailure::Worker) => "備份確認無法完成，本次操作已停止。若舊視窗仍顯示，請嘗試回覆或關閉；已逾時的回覆無效。若後續確認仍被拒絕，請重新啟動 App。",
        (false, BackupWarningFailure::Busy) => "Another backup confirmation is still pending, so a new one cannot be opened. Try responding to or closing the old dialog. Responses to timed-out operations are ignored. Restart the App if confirmation remains blocked.",
        (false, BackupWarningFailure::TimedOut) => "Backup confirmation timed out after 120 seconds and this operation stopped. Any later response to the old dialog is ignored. Try responding to or closing it if still visible; further confirmations are refused until it is released. Restart the App if it remains blocked.",
        (false, BackupWarningFailure::Disconnected) => "Backup confirmation returned no response and this operation stopped. Try responding to or closing the old dialog if still visible; responses to timed-out operations are ignored. Restart the App if confirmation remains blocked.",
        (false, BackupWarningFailure::Worker) => "Backup confirmation could not complete and this operation stopped. Try responding to or closing the old dialog if still visible; responses to timed-out operations are ignored. Restart the App if confirmation remains blocked.",
    }
    .to_string()
}

// OpenAI-Codex_backup_flow: Show filtered, bounded labels rather than complete paths.
fn backup_warning_label(input: &str) -> String {
    let leaf = input.rsplit(|c| c == '/' || c == '\\' || c == ':').next().unwrap_or("");
    let mut label = String::new();
    let mut scanned = 0usize;
    let mut displayed = 0usize;
    let mut truncated = false;
    for ch in leaf.chars() {
        if scanned == 512 {
            truncated = true;
            break;
        }
        scanned += 1;
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{206f}' | '\u{feff}')
        {
            continue;
        }
        if displayed == 80 {
            truncated = true;
            break;
        }
        label.push(ch);
        displayed += 1;
    }
    if label.is_empty() {
        label.push('—');
    }
    if truncated {
        label.push('…');
    }
    label
}

struct PendingBackupWarning {
    receiver: std::sync::mpsc::Receiver<bool>,
    deadline: std::time::Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum BackupWarningWaitError {
    TimedOut,
    Disconnected,
}

fn receive_backup_warning(pending: PendingBackupWarning) -> Result<bool, BackupWarningWaitError> {
    let remaining = pending
        .deadline
        .checked_duration_since(std::time::Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(BackupWarningWaitError::TimedOut)?;
    match pending.receiver.recv_timeout(remaining) {
        Ok(_) if std::time::Instant::now() >= pending.deadline => {
            Err(BackupWarningWaitError::TimedOut)
        }
        Ok(confirmed) => Ok(confirmed),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(BackupWarningWaitError::TimedOut),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(BackupWarningWaitError::Disconnected)
        }
    }
}

// OpenAI-Codex_backup_flow: Each prompt owns one lease and one bounded response deadline.
async fn request_backup_warning(
    app: &tauri::AppHandle,
    language: &str,
    warning: BackupWarning<'_>,
) -> Result<(bool, std::time::Instant), String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

    // The 120-second period begins before permit acquisition and builder creation.
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(120))
        .ok_or_else(|| backup_warning_failure(language, BackupWarningFailure::Worker))?;
    let guard = acquire_backup_warning(&BACKUP_WARNING_ACTIVE)
        .ok_or_else(|| backup_warning_failure(language, BackupWarningFailure::Busy))?;
    let zh = language.eq_ignore_ascii_case("zh-TW");
    let (title, body, question, ok, cancel) = match (zh, warning) {
        (true, BackupWarning::Settings) => (
            "關閉自動備份",
            "既有目標檔案可能在沒有原檔副本的情況下被覆寫。安裝或重新安裝失敗或日後解除安裝模組時，原檔可能無法還原。",
            "確定關閉自動備份？", "關閉自動備份", "取消",
        ),
        (true, BackupWarning::Install { .. }) => (
            "不備份直接安裝",
            "自動備份已關閉。既有目標檔案可能在沒有原檔副本的情況下被覆寫。安裝或重新安裝失敗或日後解除安裝模組時，原檔可能無法還原。",
            "繼續安裝？", "繼續安裝", "取消",
        ),
        (true, BackupWarning::Reinstall { .. }) => (
            "不備份直接重新安裝",
            "自動備份已關閉。既有目標檔案可能在沒有原檔副本的情況下被覆寫。安裝或重新安裝失敗或日後解除安裝模組時，原檔可能無法還原。",
            "繼續重新安裝？", "繼續重新安裝", "取消",
        ),
        (false, BackupWarning::Settings) => (
            "Turn off automatic backup",
            "Existing target files may be overwritten without a copy. If installation or reinstallation fails or the mod is later uninstalled, the originals may not be recoverable.",
            "Turn off automatic backup?", "Turn off backup", "Cancel",
        ),
        (false, BackupWarning::Install { .. }) => (
            "Install without automatic backup",
            "Automatic backup is off. Existing target files may be overwritten without a copy. If installation or reinstallation fails or the mod is later uninstalled, the originals may not be recoverable.",
            "Continue installing?", "Continue", "Cancel",
        ),
        (false, BackupWarning::Reinstall { .. }) => (
            "Reinstall without automatic backup",
            "Automatic backup is off. Existing target files may be overwritten without a copy. If installation or reinstallation fails or the mod is later uninstalled, the originals may not be recoverable.",
            "Continue reinstalling?", "Continue", "Cancel",
        ),
    };
    let labels = match (zh, warning) {
        (_, BackupWarning::Settings) => String::new(),
        (true, BackupWarning::Install { path, mode }) => format!(
            "\n\n套件與模式（已過濾標籤，上限 80 字元，過長以省略號顯示）：\n{}\n{}",
            backup_warning_label(path), backup_warning_label(mode)
        ),
        (false, BackupWarning::Install { path, mode }) => format!(
            "\n\nPackage and mode (filtered labels, maximum 80 characters, ellipsis if truncated):\n{}\n{}",
            backup_warning_label(path), backup_warning_label(mode)
        ),
        (true, BackupWarning::Reinstall { id, name }) => format!(
            "\n\n模組 ID：{id}\n名稱（已過濾標籤，上限 80 字元，過長以省略號顯示）：{}",
            backup_warning_label(name)
        ),
        (false, BackupWarning::Reinstall { id, name }) => format!(
            "\n\nMod ID: {id}\nName (filtered label, maximum 80 characters, ellipsis if truncated): {}",
            backup_warning_label(name)
        ),
    };
    let message = format!("{body}{labels}\n\n{question}");
    let (sender, receiver) = std::sync::mpsc::channel();
    let builder = app.dialog()
        .message(message)
        .title(title)
        .buttons(MessageDialogButtons::OkCancelCustom(ok.into(), cancel.into()));
    let task_guard = std::sync::Arc::clone(&guard);
    let show_task = tauri::async_runtime::spawn_blocking(move || {
        let _task_guard = task_guard;
        if std::time::Instant::now() >= deadline {
            return;
        }
        builder.show(backup_warning_callback(sender, guard));
    });
    let pending = PendingBackupWarning { receiver, deadline };
    let wait_result = tauri::async_runtime::spawn_blocking(move || receive_backup_warning(pending)).await;
    // Do not await a native show that may never return.
    drop(show_task);
    let result = wait_result
        .map_err(|_| backup_warning_failure(language, BackupWarningFailure::Worker))?;
    let confirmed = match result {
        Ok(confirmed) => confirmed,
        Err(BackupWarningWaitError::TimedOut) => {
            return Err(backup_warning_failure(language, BackupWarningFailure::TimedOut));
        }
        Err(BackupWarningWaitError::Disconnected) => {
            return Err(backup_warning_failure(language, BackupWarningFailure::Disconnected));
        }
    };
    ensure_backup_deadline(deadline, language)?;
    Ok((confirmed, deadline))
}

fn ensure_backup_deadline(deadline: std::time::Instant, language: &str) -> Result<(), String> {
    if std::time::Instant::now() >= deadline {
        return Err(backup_warning_failure(language, BackupWarningFailure::TimedOut));
    }
    Ok(())
}

fn backup_resolved_game_root(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
) -> Result<std::path::PathBuf, String> {
    let (root, _) = resolve_page_target(conn, game_path, mode)?;
    Ok(root)
}

#[derive(Debug, PartialEq, Eq)]
struct ReinstallConfirmationTarget {
    root: std::path::PathBuf,
    name: String,
    source_type: String,
    install_path: String,
    status: String,
    mode: Option<String>,
    files: Vec<(i64, String, Option<String>, String)>,
}

fn reinstall_confirmation_target(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
    mod_id: i64,
) -> Result<ReinstallConfirmationTarget, String> {
    let root = backup_resolved_game_root(conn, game_path, mode)?;
    let (name, source_type, install_path, status, saved_mode) = conn
        .query_row(
            "SELECT name, source_type, install_path, status, mode FROM installed_mod WHERE id = ?1",
            [mod_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT id, target_path, backup_path, file_hash FROM installed_mod_file \
             WHERE installed_mod_id = ?1 ORDER BY id",
        )
        .map_err(|error| error.to_string())?;
    let files = statement
        .query_map([mod_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(ReinstallConfirmationTarget {
        root, name, source_type, install_path, status, mode: saved_mode, files,
    })
}

// OpenAI-Codex_backup_flow: Preserve existing validation and logging inside the locked operation.
#[allow(clippy::too_many_arguments)]
fn run_install_with_backup(
    conn: &mut Connection,
    game_path: Option<&str>,
    mode: &str,
    path: &str,
    name: Option<&str>,
    override_foreign_conflicts: bool,
    expected_game_root: Option<&str>,
    role: Option<&str>,
    auto_backup: bool,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    check_install_preview_root(conn, game_path, mode, expected_game_root)?;
    if role.is_some_and(|value| !value.is_empty()) {
        let (_, provider) = resolve_page_target(conn, game_path, mode)?;
        crate::install_role::classify(
            std::path::Path::new(path), mode, provider.as_ref(), role,
        )?;
    }
    let backup_root =
        default_backup_root()?.join(chrono::Utc::now().format("%Y%m%d%H%M%S%3f").to_string());
    install_mod_with_role_and_backup_impl(
        conn, game_path, mode, path, name, override_foreign_conflicts,
        &backup_root, role.filter(|value| !value.is_empty()), auto_backup,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_reinstall_with_backup(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &str,
    version_label: &str,
    mode: &str,
    game_path: Option<&str>,
    auto_backup: bool,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    let backup_root =
        default_backup_root()?.join(chrono::Utc::now().format("%Y%m%d%H%M%S%3f").to_string());
    let recycle_bin_root = default_recycle_bin_root()?;
    let staging_root = default_staging_root()?;
    reinstall_mod_with_optional_staging_impl(
        conn, mod_id, new_source_path, version_label, mode, game_path,
        &backup_root, &recycle_bin_root, Some(&staging_root), auto_backup,
    )
}

#[tauri::command]
// OpenAI-Codex: Preserve the existing flat IPC payload, including optional role/root guards.
#[allow(clippy::too_many_arguments)]
pub async fn install_mod(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    game_path: Option<String>,
    mode: String,
    path: String,
    name: Option<String>,
    override_foreign_conflicts: bool,
    expected_game_root: Option<String>,
    role: Option<String>,
) -> Result<gtavmm_core::install::InstallOutcome, BackupCommandError> {
    let (snapshot, root_before) = {
        let mut conn = state.conn.lock().map_err(|error| error.to_string())?;
        let settings = load_user_settings_impl(&conn)?;
        if settings.default_auto_backup {
            return run_install_with_backup(
                &mut conn, game_path.as_deref(), &mode, &path, name.as_deref(),
                override_foreign_conflicts, expected_game_root.as_deref(), role.as_deref(), true,
            ).map_err(BackupCommandError::from);
        }
        let snapshot = backup_confirm_snapshot(&conn)?;
        if snapshot.settings != settings {
            return Err("Settings changed. Review and retry.".to_string().into());
        }
        check_install_preview_root(
            &conn, game_path.as_deref(), &mode, expected_game_root.as_deref(),
        )?;
        let root = backup_resolved_game_root(&conn, game_path.as_deref(), &mode)?;
        (snapshot, root)
    };
    let (confirmed, deadline) = request_backup_warning(
        &app, &snapshot.settings.language, BackupWarning::Install { path: &path, mode: &mode },
    ).await?;
    if !confirmed {
        return Err(backup_cancelled());
    }
    let mut conn = state.conn.lock().map_err(|error| error.to_string())?;
    ensure_backup_deadline(deadline, &snapshot.settings.language)?;
    let auto_backup = checked_backup_choice(&conn, &snapshot, confirmed)?;
    if backup_resolved_game_root(&conn, game_path.as_deref(), &mode)? != root_before {
        return Err("The game installation changed. Review and retry.".to_string().into());
    }
    ensure_backup_deadline(deadline, &snapshot.settings.language)?;
    run_install_with_backup(
        &mut conn, game_path.as_deref(), &mode, &path, name.as_deref(),
        override_foreign_conflicts, expected_game_root.as_deref(), role.as_deref(), auto_backup,
    ).map_err(BackupCommandError::from)
}

pub fn profile_list_impl(conn: &Connection) -> Result<Vec<gtavmm_core::profile::Profile>, String> {
    gtavmm_core::profile::list(conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_list(
    state: tauri::State<crate::AppState>,
) -> Result<Vec<gtavmm_core::profile::Profile>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_list_impl(&conn)
}

pub fn profile_create_impl(conn: &Connection, name: &str) -> Result<i64, String> {
    gtavmm_core::profile::create(conn, name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_create(state: tauri::State<crate::AppState>, name: String) -> Result<i64, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_create_impl(&conn, &name)
}

pub fn profile_delete_impl(conn: &Connection, profile_id: i64) -> Result<(), String> {
    gtavmm_core::profile::delete(conn, profile_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_delete(state: tauri::State<crate::AppState>, profile_id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_delete_impl(&conn, profile_id)
}

pub fn profile_mod_ids_impl(conn: &Connection, profile_id: i64) -> Result<Vec<i64>, String> {
    gtavmm_core::profile::mod_ids_in_profile(conn, profile_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_mod_ids(
    state: tauri::State<crate::AppState>,
    profile_id: i64,
) -> Result<Vec<i64>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_mod_ids_impl(&conn, profile_id)
}

pub fn profile_add_mod_impl(conn: &Connection, profile_id: i64, mod_id: i64) -> Result<(), String> {
    gtavmm_core::profile::add_mod(conn, profile_id, mod_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_add_mod(
    state: tauri::State<crate::AppState>,
    profile_id: i64,
    mod_id: i64,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_add_mod_impl(&conn, profile_id, mod_id)
}

pub fn profile_remove_mod_impl(
    conn: &Connection,
    profile_id: i64,
    mod_id: i64,
) -> Result<(), String> {
    gtavmm_core::profile::remove_mod(conn, profile_id, mod_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_remove_mod(
    state: tauri::State<crate::AppState>,
    profile_id: i64,
    mod_id: i64,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_remove_mod_impl(&conn, profile_id, mod_id)
}

/// Switches the active profile. `staging_root` is a parameter (not computed
/// internally) for the same testability reason as `install_mod_impl`.
pub fn profile_switch_impl(
    conn: &Connection,
    profile_id: i64,
    staging_root: &std::path::Path,
) -> Result<gtavmm_core::profile::SwitchOutcome, String> {
    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    let result = gtavmm_core::profile::switch(conn, profile_id, staging_root);
    if matches!(&result, Err(gtavmm_core::CoreError::ModDisablingUnavailable)) {
        return Err("GTAVMM_PROFILE_MOD_DISABLING_UNAVAILABLE".to_string());
    }
    let result = result.map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!(
            "profile_switch failed for profile #{profile_id}: {reason}"
        ));
    }
    result
}

#[tauri::command]
pub fn profile_switch(
    state: tauri::State<crate::AppState>,
    profile_id: i64,
) -> Result<gtavmm_core::profile::SwitchOutcome, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let staging_root = default_staging_root()?;
    profile_switch_impl(&conn, profile_id, &staging_root)
}

pub fn get_language_impl(conn: &Connection) -> Result<String, String> {
    gtavmm_core::settings::load(conn)
        .map(|s| s.language)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_language(state: tauri::State<crate::AppState>) -> Result<String, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    get_language_impl(&conn)
}

pub fn set_language_impl(conn: &Connection, language: &str) -> Result<(), String> {
    let mut settings = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    settings.language = language.to_string();
    gtavmm_core::settings::save(conn, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_language(state: tauri::State<crate::AppState>, language: String) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    set_language_impl(&conn, &language)
}

pub fn inspect_dll_impl(
    dll_path: &str,
) -> Result<gtavmm_core::dll_translation::DllInspectionSnapshot, String> {
    gtavmm_core::dll_translation::inspect_with_fingerprint(std::path::Path::new(dll_path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn inspect_dll(
    dll_path: String,
) -> Result<gtavmm_core::dll_translation::DllInspectionSnapshot, String> {
    inspect_dll_impl(&dll_path)
}

// OpenAI-Codex_Responsible: Source_Code

pub fn translate_dll_draft_impl(
    conn: &Connection,
    dll_path: &str,
    target_language: &str,
    expected_sha256: &str,
) -> Result<Vec<gtavmm_core::dll_translation::TranslatedDraftEntry>, String> {
    let target_language = gtavmm_core::translation::validate_target_language(target_language)?;
    let result = gtavmm_core::dll_translation::translate_draft_for_source(
        conn,
        std::path::Path::new(dll_path),
        target_language,
        expected_sha256,
        15,
    )
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!(
            "translate_dll_draft failed for '{dll_path}': {reason}"
        ));
    }
    result
}

/// Step 1 of the review flow: AI-translates every candidate string but writes nothing
/// — the frontend shows these to the user (editable) before [`patch_dll_translations`]
/// commits anything.
#[tauri::command]
pub fn translate_dll_draft(
    state: tauri::State<crate::AppState>,
    dll_path: String,
    target_language: String,
    expected_sha256: String,
) -> Result<Vec<gtavmm_core::dll_translation::TranslatedDraftEntry>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    translate_dll_draft_impl(&conn, &dll_path, &target_language, &expected_sha256)
}

pub fn patch_dll_translations_impl(
    dll_path: &str,
    target_language: &str,
    expected_sha256: &str,
    translations: Vec<String>,
    output_path: Option<&str>,
) -> Result<gtavmm_core::dll_translation::DllTranslationOutcome, String> {
    let target_language = gtavmm_core::translation::validate_target_language(target_language)?;
    let output_path = output_path.map(std::path::Path::new);
    let result = gtavmm_core::dll_translation::patch_with_translations_for_source(
        std::path::Path::new(dll_path),
        target_language,
        expected_sha256,
        &translations,
        output_path,
    )
    .map_err(|e| e.to_string());
    match &result {
        Ok(outcome) => {
            let _ = gtavmm_core::app_log::info(&format!(
                "patch_dll_translations wrote {} (strings_translated={}, call_sites_patched={})",
                outcome.output_path.display(),
                outcome.strings_translated,
                outcome.call_sites_patched
            ));
        }
        Err(reason) => {
            let _ = gtavmm_core::app_log::error(&format!(
                "patch_dll_translations failed for '{dll_path}': {reason}"
            ));
        }
    }
    result
}

/// Step 2 of the review flow (also the entire flow for a fully manual translation,
/// since this never calls the AI assistant itself): patches the given translations —
/// AI drafts the user may have edited, or text they typed in by hand — into a new copy
/// of the DLL.
#[tauri::command]
pub fn patch_dll_translations(
    dll_path: String,
    target_language: String,
    expected_sha256: String,
    translations: Vec<String>,
    output_path: Option<String>,
) -> Result<gtavmm_core::dll_translation::DllTranslationOutcome, String> {
    patch_dll_translations_impl(
        &dll_path,
        &target_language,
        &expected_sha256,
        translations,
        output_path.as_deref(),
    )
}

/// OpenAI-Codex: Generate config drafts without overwriting an existing sibling file.
pub fn generate_config_translation_draft_impl(
    conn: &Connection,
    source_path: &str,
    target_language: &str,
) -> Result<String, String> {
    let target_language = gtavmm_core::translation::validate_target_language(target_language)?;

    let source_path = std::path::Path::new(source_path);
    let extension = source_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("ini") && !extension.eq_ignore_ascii_case("xml") {
        return Err("config translation only supports .ini and .xml files".to_string());
    }

    let stem = source_path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            format!(
                "could not determine a file name for {}",
                source_path.display()
            )
        })?;
    let output_path = source_path.with_file_name(format!("{stem}.{target_language}.{extension}"));
    match std::fs::symlink_metadata(&output_path) {
        Ok(_) => {
            return Err(format!(
                "translation draft already exists at {}; move it or choose another target language",
                output_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    gtavmm_core::translation::generate_draft(conn, source_path, target_language)
        .map(|path| path.display().to_string())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn generate_config_translation_draft(
    state: tauri::State<crate::AppState>,
    source_path: String,
    target_language: String,
) -> Result<String, String> {
    let conn = state.conn.lock().map_err(|error| error.to_string())?;
    generate_config_translation_draft_impl(&conn, &source_path, &target_language)
}

// ---------------------------------------------------------------------------
// Activity log (gtavmm_core::history) — read-only viewer for install/uninstall/
// enable/disable/restore events, previously only queryable from the CLI.
// ---------------------------------------------------------------------------

pub fn list_history_impl(
    conn: &Connection,
    mod_id: Option<i64>,
    mode: Option<&str>,
) -> Result<Vec<gtavmm_core::db::models::InstallEvent>, String> {
    gtavmm_core::history::list_filtered(conn, mod_id, mode).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_history(
    state: tauri::State<crate::AppState>,
    mod_id: Option<i64>,
    mode: Option<String>,
) -> Result<Vec<gtavmm_core::db::models::InstallEvent>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    list_history_impl(&conn, mod_id, mode.as_deref())
}

// ---------------------------------------------------------------------------
// Saved mod links (gtavmm_core::saved_links) — standalone bookmarks, independent
// of installed_mod.
// ---------------------------------------------------------------------------

pub fn list_saved_links_impl(
    conn: &Connection,
) -> Result<Vec<gtavmm_core::saved_links::SavedModLink>, String> {
    gtavmm_core::saved_links::list(conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_saved_links(
    state: tauri::State<crate::AppState>,
) -> Result<Vec<gtavmm_core::saved_links::SavedModLink>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    list_saved_links_impl(&conn)
}

pub fn add_saved_link_impl(
    conn: &Connection,
    name: &str,
    url: &str,
    notes: Option<&str>,
) -> Result<i64, String> {
    gtavmm_core::saved_links::add(conn, name, url, notes).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_saved_link(
    state: tauri::State<crate::AppState>,
    name: String,
    url: String,
    notes: Option<String>,
) -> Result<i64, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    add_saved_link_impl(&conn, &name, &url, notes.as_deref())
}

pub fn update_saved_link_impl(
    conn: &Connection,
    id: i64,
    name: &str,
    url: &str,
    notes: Option<&str>,
) -> Result<(), String> {
    gtavmm_core::saved_links::update(conn, id, name, url, notes).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_saved_link(
    state: tauri::State<crate::AppState>,
    id: i64,
    name: String,
    url: String,
    notes: Option<String>,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    update_saved_link_impl(&conn, id, &name, &url, notes.as_deref())
}

pub fn delete_saved_link_impl(conn: &Connection, id: i64) -> Result<(), String> {
    gtavmm_core::saved_links::delete(conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_saved_link(state: tauri::State<crate::AppState>, id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    delete_saved_link_impl(&conn, id)
}

// ---------------------------------------------------------------------------
// Mod details (gtavmm_core::mod_details) — editing an installed mod's own
// notes/link fields, previously read-only.
// ---------------------------------------------------------------------------

pub fn update_mod_details_impl(
    conn: &Connection,
    mod_id: i64,
    notes: Option<&str>,
    link: Option<&str>,
) -> Result<(), String> {
    gtavmm_core::mod_details::update(conn, mod_id, notes, link).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_mod_details(
    state: tauri::State<crate::AppState>,
    mod_id: i64,
    notes: Option<String>,
    link: Option<String>,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    update_mod_details_impl(&conn, mod_id, notes.as_deref(), link.as_deref())
}

// ---------------------------------------------------------------------------
// App diagnostic log (gtavmm_core::app_log) — a plain local log file the user can
// look at (or attach to a bug report) without digging through the OS app-data
// directory themselves.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn read_app_log(max_lines: usize) -> Result<Vec<String>, String> {
    gtavmm_core::app_log::read_recent(max_lines).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn app_log_path() -> Option<String> {
    gtavmm_core::app_log::log_path().map(|p| p.display().to_string())
}

/// Manual "Clear Log Now" — resets the same 3-day timer the automatic cleanup uses, so
/// clearing by hand doesn't leave the next automatic cleanup stacked right behind it.
#[tauri::command]
pub fn clear_app_log() -> Result<(), String> {
    gtavmm_core::app_log::clear().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn app_log_last_cleanup() -> Option<String> {
    gtavmm_core::app_log::last_cleanup().map(|dt| dt.to_rfc3339())
}

// ---------------------------------------------------------------------------
// AI Assistant (gtavmm_core::ai_assistant) — opt-in crash/error diagnosis. Was
// previously reachable only from the CLI (`gtavmm ai ...`) despite already being
// tested end-to-end against a real cloud provider; this wires it into the GUI.
// ---------------------------------------------------------------------------

// OpenAI-Codex: Keep the desktop provider IDs aligned with the core adapters.
fn parse_provider_kind(
    provider: &str,
) -> Result<gtavmm_core::ai_assistant::AiProviderKind, String> {
    match provider {
        "ollama" => Ok(gtavmm_core::ai_assistant::AiProviderKind::Ollama),
        "ollama_cloud" => Ok(gtavmm_core::ai_assistant::AiProviderKind::OllamaCloud),
        "openai" => Ok(gtavmm_core::ai_assistant::AiProviderKind::OpenAI),
        "anthropic" => Ok(gtavmm_core::ai_assistant::AiProviderKind::Anthropic),
        "deepseek" => Ok(gtavmm_core::ai_assistant::AiProviderKind::DeepSeek),
        "openrouter" => Ok(gtavmm_core::ai_assistant::AiProviderKind::OpenRouter),
        "google_ai" => Ok(gtavmm_core::ai_assistant::AiProviderKind::GoogleAI),
        "nvidia" => Ok(gtavmm_core::ai_assistant::AiProviderKind::Nvidia),
        "cloud" => Ok(gtavmm_core::ai_assistant::AiProviderKind::Cloud),
        other => Err(format!("unknown AI provider kind: {other}")),
    }
}

pub fn ai_load_settings_impl(
    conn: &Connection,
) -> Result<gtavmm_core::ai_assistant::AiSettings, String> {
    gtavmm_core::ai_assistant::load_settings(conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ai_load_settings(
    state: tauri::State<crate::AppState>,
) -> Result<gtavmm_core::ai_assistant::AiSettings, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    ai_load_settings_impl(&conn)
}

pub fn ai_enable_impl(
    conn: &Connection,
    provider: &str,
    model: Option<String>,
    cloud_endpoint: Option<String>,
) -> Result<(), String> {
    let provider = parse_provider_kind(provider)?;
    gtavmm_core::ai_assistant::enable(conn, provider, model, cloud_endpoint)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ai_enable(
    state: tauri::State<crate::AppState>,
    provider: String,
    model: Option<String>,
    cloud_endpoint: Option<String>,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    ai_enable_impl(&conn, &provider, model, cloud_endpoint)
}

pub fn ai_disable_impl(conn: &Connection) -> Result<(), String> {
    gtavmm_core::ai_assistant::disable(conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ai_disable(state: tauri::State<crate::AppState>) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    ai_disable_impl(&conn)
}

/// Stores the cloud API key in the OS-native credential store — never in the
/// database or any file this app writes.
#[tauri::command]
pub fn ai_set_cloud_api_key(key: String) -> Result<(), String> {
    gtavmm_core::ai_assistant::set_cloud_api_key(&key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ai_has_cloud_api_key() -> bool {
    gtavmm_core::ai_assistant::has_cloud_api_key()
}

#[tauri::command]
pub fn ai_set_provider_api_key(provider: String, key: String) -> Result<(), String> {
    let provider = parse_provider_kind(&provider)?;
    gtavmm_core::ai_assistant::set_provider_api_key(provider, &key)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn ai_has_provider_api_key(provider: String) -> Result<bool, String> {
    let provider = parse_provider_kind(&provider)?;
    Ok(gtavmm_core::ai_assistant::has_provider_api_key(provider))
}

#[tauri::command]
pub fn ai_ollama_available() -> bool {
    gtavmm_core::ai_assistant::ollama_available()
}

pub fn ai_diagnose_impl(conn: &Connection, raw_context: &str) -> Result<String, String> {
    let result = gtavmm_core::ai_assistant::diagnose(conn, raw_context).map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!("ai_diagnose failed: {reason}"));
    }
    result
}

#[tauri::command]
pub fn ai_diagnose(
    state: tauri::State<crate::AppState>,
    raw_context: String,
) -> Result<String, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    ai_diagnose_impl(&conn, &raw_context)
}

// ---------------------------------------------------------------------------
// Tools: Component Checker (gtavmm_core::components), full mods\ folder backup
// (gtavmm_core::full_backup), and the uninstall recycle bin (gtavmm_core::
// recycle_bin) — all three were previously CLI-only despite being fully built
// and tested.
// ---------------------------------------------------------------------------

/// The application-data directory the database lives in. Everything the engine
/// writes outside the game folder — backups, staging, the recycle bin — hangs
/// off this, so resolving it in one place keeps those three from drifting apart
/// as more commands need them.
fn app_data_root() -> Result<std::path::PathBuf, String> {
    let db_path = gtavmm_core::db::default_db_path()
        .ok_or_else(|| "could not resolve an app-data directory on this OS".to_string())?;
    Ok(db_path
        .parent()
        .expect("db path always has a parent")
        .to_path_buf())
}

fn default_backup_root() -> Result<std::path::PathBuf, String> {
    Ok(app_data_root()?.join("backups"))
}

/// Where a disabled mod's files are parked. `state::disable` moves deployed
/// files here and `state::enable` moves them back, so both sides must agree on
/// this path or an enable would find nothing to restore.
fn default_staging_root() -> Result<std::path::PathBuf, String> {
    Ok(app_data_root()?.join("staging"))
}

/// OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code
/// Root used when uninstall attempts to record a recycle-bin snapshot.
/// Restoration is not guaranteed. This version pauses automatic expiry sweeping.
fn default_recycle_bin_root() -> Result<std::path::PathBuf, String> {
    Ok(app_data_root()?.join("recycle_bin"))
}

pub fn check_components_impl(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
) -> Result<Vec<gtavmm_core::components::ComponentStatus>, String> {
    let (game_root, _provider) = resolve_page_target(conn, game_path, mode)?;
    Ok(gtavmm_core::components::check_all(&game_root))
}

/// Writes a profile to a JSON file.
///
/// The file names mods, it does not contain them — this project never
/// redistributes mod files. Importing it elsewhere therefore matches names
/// against what is installed there and reports what is missing rather than
/// fetching anything.
pub fn profile_export_impl(
    conn: &Connection,
    profile_id: i64,
    output_path: &str,
) -> Result<(), String> {
    let export = gtavmm_core::profile::export(conn, profile_id).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&export).map_err(|e| e.to_string())?;
    std::fs::write(output_path, json).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_export(
    state: tauri::State<crate::AppState>,
    profile_id: i64,
    output_path: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_export_impl(&conn, profile_id, &output_path)
}

pub fn profile_import_impl(
    conn: &Connection,
    input_path: &str,
) -> Result<gtavmm_core::profile::ImportOutcome, String> {
    let text = std::fs::read_to_string(input_path).map_err(|e| e.to_string())?;
    let export: gtavmm_core::profile::ProfileExport =
        serde_json::from_str(&text).map_err(|e| format!("not a profile export file: {e}"))?;
    gtavmm_core::profile::import(conn, &export).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn profile_import(
    state: tauri::State<crate::AppState>,
    input_path: String,
) -> Result<gtavmm_core::profile::ImportOutcome, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    profile_import_impl(&conn, &input_path)
}

/// The LSPDFR framework panel: RPH, LSPDFR and RAGENativeUI.
pub fn check_framework_impl(
    conn: &Connection,
    game_path: Option<&str>,
    mode: &str,
) -> Result<Vec<gtavmm_core::components::ComponentStatus>, String> {
    let page = crate::page_mode::PageMode::parse(mode)?;
    if !matches!(
        page,
        crate::page_mode::PageMode::LegacyLspdfr | crate::page_mode::PageMode::EnhancedLspdfr
    ) {
        return Err("framework checks require an LSPDFR page".to_string());
    }
    let (game_root, _provider) = resolve_page_target(conn, game_path, mode)?;
    Ok(gtavmm_core::components::check_set(
        &game_root,
        gtavmm_core::components::ComponentSet::LspdfrFramework,
    ))
}

#[tauri::command]
pub fn check_framework(
    state: tauri::State<crate::AppState>,
    game_path: Option<String>,
    mode: String,
) -> Result<Vec<gtavmm_core::components::ComponentStatus>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    check_framework_impl(&conn, game_path.as_deref(), &mode)
}

/// Records an LSPDFR category chosen by hand, correcting whatever was guessed.
pub fn set_mod_category_impl(conn: &Connection, mod_id: i64, category: &str) -> Result<(), String> {
    const KNOWN: [&str; 5] = ["callouts", "eup-peds", "vehicles", "other", "framework"];
    if !KNOWN.contains(&category) {
        return Err(format!("unknown category: {category}"));
    }
    let changed = conn
        .execute(
            "UPDATE installed_mod SET category = ?1 WHERE id = ?2",
            rusqlite::params![category, mod_id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("no installed mod with id {mod_id}"));
    }
    Ok(())
}

#[tauri::command]
pub fn set_mod_category(
    state: tauri::State<crate::AppState>,
    mod_id: i64,
    category: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    set_mod_category_impl(&conn, mod_id, &category)
}

#[tauri::command]
pub fn check_components(
    state: tauri::State<crate::AppState>,
    game_path: Option<String>,
    mode: String,
) -> Result<Vec<gtavmm_core::components::ComponentStatus>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    check_components_impl(&conn, game_path.as_deref(), &mode)
}

fn full_backup_edition_for_mode(
    mode: &str,
) -> Result<gtavmm_core::full_backup::GameEdition, String> {
    let page = crate::page_mode::PageMode::parse(mode)?;
    page.ensure_available()?;
    match page.expected_game_edition() {
        Some("legacy") => Ok(gtavmm_core::full_backup::GameEdition::Legacy),
        Some("enhanced") => Ok(gtavmm_core::full_backup::GameEdition::Enhanced),
        _ => Err("Full backups require a GTA V SP or LSPDFR page.".to_string()),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FullBackupListResult {
    pub paths: Vec<String>,
    pub unmarked_count: usize,
    pub unreadable_count: usize,
}

pub fn create_full_backup_impl(
    game_root: &std::path::Path,
    edition: gtavmm_core::full_backup::GameEdition,
) -> Result<String, String> {
    let backup_root = default_backup_root()?;
    let result = gtavmm_core::full_backup::create_for_edition(game_root, &backup_root, edition)
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!("create_full_backup failed: {reason}"));
    }
    result
}

#[tauri::command]
pub fn create_full_backup(
    state: tauri::State<crate::AppState>,
    game_path: Option<String>,
    mode: String,
) -> Result<String, String> {
    let edition = full_backup_edition_for_mode(&mode)?;
    let game_root = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        resolve_page_target(&conn, game_path.as_deref(), &mode)?.0
    };
    create_full_backup_impl(&game_root, edition)
}

pub fn list_full_backups_impl(mode: &str) -> Result<FullBackupListResult, String> {
    let edition = full_backup_edition_for_mode(mode)?;
    let backup_root = default_backup_root()?;
    gtavmm_core::full_backup::list_for_edition(&backup_root, edition)
        .map(|listing| FullBackupListResult {
            paths: listing
                .paths
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
            unmarked_count: listing.unmarked_count,
            unreadable_count: listing.unreadable_count,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_full_backups(mode: String) -> Result<FullBackupListResult, String> {
    list_full_backups_impl(&mode)
}

pub fn restore_full_backup_impl(
    zip_path: &str,
    game_root: &std::path::Path,
    edition: gtavmm_core::full_backup::GameEdition,
) -> Result<(), String> {
    let result = gtavmm_core::full_backup::restore_for_edition(
        std::path::Path::new(zip_path),
        game_root,
        edition,
    )
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!(
            "restore_full_backup failed for '{zip_path}': {reason}"
        ));
    }
    result
}

#[tauri::command]
pub fn restore_full_backup(
    state: tauri::State<crate::AppState>,
    zip_path: String,
    game_path: Option<String>,
    mode: String,
) -> Result<(), String> {
    let edition = full_backup_edition_for_mode(&mode)?;
    let game_root = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        resolve_page_target(&conn, game_path.as_deref(), &mode)?.0
    };
    restore_full_backup_impl(&zip_path, &game_root, edition)
}

pub fn list_recycle_bin_impl(
    conn: &Connection,
) -> Result<Vec<gtavmm_core::recycle_bin::RecycleBinEntry>, String> {
    list_recycle_bin_for_mode_impl(conn, None)
}

pub fn list_recycle_bin_for_mode_impl(
    conn: &Connection,
    mode: Option<&str>,
) -> Result<Vec<gtavmm_core::recycle_bin::RecycleBinEntry>, String> {
    gtavmm_core::recycle_bin::list_for_mode(conn, mode).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_recycle_bin(
    state: tauri::State<crate::AppState>,
    mode: Option<String>,
) -> Result<Vec<gtavmm_core::recycle_bin::RecycleBinEntry>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    match mode.as_deref() {
        Some(mode) => list_recycle_bin_for_mode_impl(&conn, Some(mode)),
        None => list_recycle_bin_impl(&conn),
    }
}

pub fn restore_recycle_bin_entry_impl(
    conn: &mut Connection,
    entry_id: i64,
    game_path: Option<&str>,
) -> Result<(), String> {
    let has_recorded_root =
        gtavmm_core::recycle_bin::has_recorded_snapshot_metadata(conn, entry_id)
            .map_err(|e| e.to_string())?;
    let fallback_game_root = if has_recorded_root {
        None
    } else {
        Some(
            gtavmm_core::providers::resolve_game_root(game_path.map(std::path::Path::new))
                .map_err(|e| e.to_string())?
                .0,
        )
    };
    let backup_root = default_backup_root()?;
    // OpenAI-Codex: Supply storage roots independently of the snapshot record.
    let recycle_root = default_recycle_bin_root()?;
    let staging_root = default_staging_root()?;
    let result = gtavmm_core::recycle_bin::restore_with_recycle_root(
        conn,
        entry_id,
        fallback_game_root.as_deref(),
        &backup_root,
        &recycle_root,
        &staging_root,
    )
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ = gtavmm_core::app_log::error(&format!(
            "restore_recycle_bin_entry failed for entry #{entry_id}: {reason}"
        ));
    }
    result
}

#[tauri::command]
pub fn restore_recycle_bin_entry(
    state: tauri::State<crate::AppState>,
    entry_id: i64,
    game_path: Option<String>,
) -> Result<(), String> {
    let mut conn = state.conn.lock().map_err(|e| e.to_string())?;
    restore_recycle_bin_entry_impl(&mut conn, entry_id, game_path.as_deref())
}

pub fn sweep_expired_recycle_bin_impl(conn: &Connection) -> Result<usize, String> {
    gtavmm_core::recycle_bin::sweep_expired_with_root(
        conn,
        &default_recycle_bin_root()?,
        &default_backup_root()?,
        &default_staging_root()?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn sweep_expired_recycle_bin(state: tauri::State<crate::AppState>) -> Result<usize, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    sweep_expired_recycle_bin_impl(&conn)
}

// ---------------------------------------------------------------------------
// AI Workflow / Prompt template library (gtavmm_core::prompt_template) — the
// user's own reusable prompt text, plain CRUD, not part of the Action Schema.
// ---------------------------------------------------------------------------

pub fn list_prompt_templates_impl(
    conn: &Connection,
) -> Result<Vec<gtavmm_core::prompt_template::PromptTemplate>, String> {
    gtavmm_core::prompt_template::list(conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_prompt_templates(
    state: tauri::State<crate::AppState>,
) -> Result<Vec<gtavmm_core::prompt_template::PromptTemplate>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    list_prompt_templates_impl(&conn)
}

pub fn add_prompt_template_impl(
    conn: &Connection,
    name: &str,
    content: &str,
) -> Result<i64, String> {
    gtavmm_core::prompt_template::create(conn, name, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_prompt_template(
    state: tauri::State<crate::AppState>,
    name: String,
    content: String,
) -> Result<i64, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    add_prompt_template_impl(&conn, &name, &content)
}

pub fn update_prompt_template_impl(
    conn: &Connection,
    id: i64,
    name: &str,
    content: &str,
) -> Result<(), String> {
    gtavmm_core::prompt_template::update(conn, id, name, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_prompt_template(
    state: tauri::State<crate::AppState>,
    id: i64,
    name: String,
    content: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    update_prompt_template_impl(&conn, id, &name, &content)
}

pub fn delete_prompt_template_impl(conn: &Connection, id: i64) -> Result<(), String> {
    gtavmm_core::prompt_template::delete(conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_prompt_template(state: tauri::State<crate::AppState>, id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    delete_prompt_template_impl(&conn, id)
}

// ---------------------------------------------------------------------------
// Export to Excel (gtavmm_core::xlsx_export) — was CLI-only (`gtavmm export`),
// missed in the first pass over unwired core modules; the design's SP Mods page
// has always had an "Export to Excel" button, but nothing backed it.
// ---------------------------------------------------------------------------

pub fn export_mods_to_xlsx_impl(
    conn: &Connection,
    output_path: &str,
    mode: &str,
) -> Result<(), String> {
    let page = crate::page_mode::PageMode::parse(mode)?;
    if page != crate::page_mode::PageMode::LegacySp {
        return Err("Excel export is only available for the Legacy SP page".to_string());
    }
    gtavmm_core::xlsx_export::export_for_mode(
        conn,
        std::path::Path::new(output_path),
        page.as_str(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_mods_to_xlsx(
    state: tauri::State<crate::AppState>,
    output_path: String,
    mode: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    export_mods_to_xlsx_impl(&conn, &output_path, &mode)
}

// ---------------------------------------------------------------------------
// Tools page: File Editor (plain-text .ini/.xml/.json, no backup — matches the
// design's own disclaimer) and Hash Calculator (gtavmm_core::hash_calculator).
// ---------------------------------------------------------------------------

const MAX_TEXT_FILE_BYTES: u64 = 5 * 1024 * 1024;

// OpenAI-Codex_atomic_review: The editor exports a new XML file; it never edits archives.
#[derive(Serialize)]
pub struct DlcEditorDocument {
    source_path: String,
    source_hash: String,
    content: String,
    validation: gtavmm_core::dlclist_editor::DlcEditorValidation,
}

#[derive(Serialize)]
pub struct DlcEditorAnalysis {
    validation: gtavmm_core::dlclist_editor::DlcEditorValidation,
    diff: gtavmm_core::dlclist_editor::DlcEditorDiff,
}

// OpenAI-Codex: Confirmation contains public request data only, never credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlcAiPreview {
    provider: gtavmm_core::ai_assistant::DlcAiProviderSnapshot,
    user_prompt: String,
}

#[derive(Serialize)]
pub struct DlcAiResult {
    content: String,
    analysis: DlcEditorAnalysis,
}

fn prepare_dlclist_ai_impl(
    settings: &gtavmm_core::ai_assistant::AiSettings,
    content: &str,
    instruction: &str,
) -> Result<DlcAiPreview, String> {
    Ok(DlcAiPreview {
        provider: gtavmm_core::ai_assistant::dlc_provider_snapshot(settings)
            .map_err(|e| e.to_string())?,
        user_prompt: gtavmm_core::dlclist_editor::build_dlclist_ai_prompt(content, instruction)
            .map_err(|e| e.to_string())?,
    })
}

fn check_dlclist_ai_preview(
    settings: &gtavmm_core::ai_assistant::AiSettings,
    content: &str,
    instruction: &str,
    expected: &DlcAiPreview,
) -> Result<DlcAiPreview, String> {
    let current = prepare_dlclist_ai_impl(settings, content, instruction)?;
    if current != *expected {
        return Err("AI settings or input changed. Prepare and review a new preview.".into());
    }
    Ok(current)
}

fn dlclist_ai_result(content: &str, response: String) -> Result<DlcAiResult, String> {
    let validation = gtavmm_core::dlclist_editor::validate_dlclist_ai_response(content, &response)
        .map_err(|e| e.to_string())?;
    let diff = gtavmm_core::dlclist_editor::compare_dlclist(content, &response)
        .map_err(|e| e.to_string())?;
    Ok(DlcAiResult {
        content: response,
        analysis: DlcEditorAnalysis { validation, diff },
    })
}

#[tauri::command]
pub fn prepare_dlclist_ai(
    state: tauri::State<crate::AppState>,
    content: String,
    instruction: String,
) -> Result<DlcAiPreview, String> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| "AI settings are unavailable.".to_string())?;
    let settings = gtavmm_core::ai_assistant::load_settings(&conn)
        .map_err(|_| "AI settings are unavailable.".to_string())?;
    prepare_dlclist_ai_impl(&settings, &content, &instruction)
}

#[tauri::command]
pub async fn suggest_dlclist_ai(
    state: tauri::State<'_, crate::AppState>,
    content: String,
    instruction: String,
    expected: DlcAiPreview,
) -> Result<DlcAiResult, String> {
    let confirmed = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| "AI settings are unavailable.".to_string())?;
        let settings = gtavmm_core::ai_assistant::load_settings(&conn)
            .map_err(|_| "AI settings are unavailable.".to_string())?;
        check_dlclist_ai_preview(&settings, &content, &instruction, &expected)?
    };
    // The immutable confirmed destination survives setting changes; no DB lock crosses HTTP.
    tauri::async_runtime::spawn_blocking(move || {
        let response = gtavmm_core::ai_assistant::call_dlclist_provider(
            &confirmed.provider,
            &confirmed.user_prompt,
        )
        .map_err(|e| e.to_string())?;
        dlclist_ai_result(&content, response)
    })
    .await
    .map_err(|_| "AI draft request could not complete.".to_string())?
}

fn dlclist_path_checks(path: &str, existing: bool) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Path};
    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("An absolute XML path without relative components is required.".into());
    }
    // OpenAI-Codex: An export must be a regular file, never a Windows data stream.
    #[cfg(windows)]
    if path.components().any(|component| matches!(component, Component::Normal(name) if name.to_string_lossy().contains(':'))) {
        return Err("Alternate data streams are not supported.".into());
    }
    if !path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("xml"))
        || gtavmm_core::protected_files::is_protected(path)
    {
        return Err("A non-protected XML path is required.".into());
    }
    if path.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("update.rpf")
    }) {
        return Err("Paths inside update.rpf are not supported; use an exported XML copy.".into());
    }
    for ancestor in path.ancestors().skip(1) {
        let metadata = std::fs::symlink_metadata(ancestor)
            .map_err(|e| format!("Unable to inspect parent: {e}"))?;
        if dlclist_linked(&metadata) || !metadata.is_dir() {
            return Err("Linked or non-directory parents are not allowed.".into());
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if existing && !dlclist_linked(&metadata) && metadata.is_file() => {}
        Ok(_) if existing => return Err("Source must be a regular, non-linked file.".into()),
        Ok(_) => return Err("Output already exists.".into()),
        Err(e) if !existing && e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Unable to inspect path: {e}")),
    }
    Ok(path.to_path_buf())
}

fn dlclist_linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn read_dlclist_snapshot(path: &str) -> Result<(String, String), String> {
    let path = dlclist_path_checks(path, true)?;
    let file = std::fs::File::open(&path).map_err(|e| format!("Unable to open source: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Source is not a regular file.".into());
    }
    let mut bytes = Vec::new();
    file.take((gtavmm_core::dlclist_editor::MAX_DLCLIST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Unable to read source: {e}"))?;
    if bytes.len() > gtavmm_core::dlclist_editor::MAX_DLCLIST_BYTES {
        return Err("Source exceeds the 5 MiB limit.".into());
    }
    let hash = gtavmm_core::dlclist_editor::sha256_bytes(&bytes);
    let content = String::from_utf8(bytes).map_err(|_| "Source is not valid UTF-8.".to_string())?;
    Ok((content, hash))
}

fn read_dlclist_document_impl(path: &str) -> Result<DlcEditorDocument, String> {
    let (content, source_hash) = read_dlclist_snapshot(path)?;
    let validation =
        gtavmm_core::dlclist_editor::validate_dlclist(&content).map_err(|e| e.to_string())?;
    Ok(DlcEditorDocument {
        source_path: path.to_string(),
        source_hash,
        content,
        validation,
    })
}

#[tauri::command]
pub fn read_dlclist_document(path: String) -> Result<DlcEditorDocument, String> {
    read_dlclist_document_impl(&path)
}

#[tauri::command]
pub fn validate_dlclist_draft(
    original: String,
    draft: String,
) -> Result<DlcEditorAnalysis, String> {
    let validation =
        gtavmm_core::dlclist_editor::validate_dlclist(&draft).map_err(|e| e.to_string())?;
    let diff = gtavmm_core::dlclist_editor::compare_dlclist(&original, &draft)
        .map_err(|e| e.to_string())?;
    Ok(DlcEditorAnalysis { validation, diff })
}

fn save_dlclist_copy_impl(
    source_path: &str,
    expected_source_hash: &str,
    output_path: &str,
    content: &str,
) -> Result<String, String> {
    gtavmm_core::dlclist_editor::validate_dlclist(content).map_err(|e| e.to_string())?;
    let (source_content, source_hash) = read_dlclist_snapshot(source_path)?;
    gtavmm_core::dlclist_editor::validate_dlclist(&source_content).map_err(|e| e.to_string())?;
    if source_hash != expected_source_hash {
        return Err("Source changed since it was opened; reopen it before exporting.".into());
    }
    let output = dlclist_path_checks(output_path, false)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|e| format!("Unable to create output: {e}"))?;
    file.write_all(content.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| {
            format!(
                "Export failed; partial output may remain at {}: {e}",
                output.display()
            )
        })?;
    Ok(output_path.to_string())
}

#[tauri::command]
pub fn save_dlclist_copy(
    source_path: String,
    expected_source_hash: String,
    output_path: String,
    content: String,
) -> Result<String, String> {
    save_dlclist_copy_impl(&source_path, &expected_source_hash, &output_path, &content)
}
const EDITABLE_TEXT_EXTENSIONS: &[&str] = &["ini", "xml", "json", "meta", "cfg", "txt"];

fn validate_editable_path(path: &str, must_exist: bool) -> Result<std::path::PathBuf, String> {
    if path.is_empty() {
        return Err("A file path is required.".to_string());
    }

    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err("Only absolute file paths can be edited.".to_string());
    }

    if gtavmm_core::protected_files::is_protected(path) {
        return Err("Protected files cannot be edited.".to_string());
    }

    let extension = path.extension().and_then(std::ffi::OsStr::to_str);
    if !extension.is_some_and(|extension| {
        EDITABLE_TEXT_EXTENSIONS
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(extension))
    }) {
        return Err("The file extension is not supported by the text editor.".to_string());
    }

    for ancestor in path.ancestors().skip(1) {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Symbolic links in file paths cannot be edited.".to_string());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !must_exist => {}
            Err(error) => return Err(format!("Unable to inspect the file path: {error}")),
        }
    }

    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err("Symbolic links cannot be edited.".to_string());
            }
            if !metadata.is_file() {
                return Err("The path must point to an existing regular file.".to_string());
            }
            if metadata.len() > MAX_TEXT_FILE_BYTES {
                return Err("The file exceeds the 5 MiB editing limit.".to_string());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !must_exist => {
            return Ok(path.to_path_buf());
        }
        Err(error) => return Err(format!("Unable to inspect the file path: {error}")),
    }

    Ok(path.to_path_buf())
}

fn read_text_file_impl(path: &str) -> Result<String, String> {
    let path = validate_editable_path(path, true)?;
    let file =
        std::fs::File::open(&path).map_err(|error| format!("Unable to open the file: {error}"))?;
    let mut limited_file = file.take(MAX_TEXT_FILE_BYTES + 1);
    let mut bytes = Vec::new();
    limited_file
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Unable to read the file: {error}"))?;
    if bytes.len() as u64 > MAX_TEXT_FILE_BYTES {
        return Err("The file exceeds the 5 MiB editing limit.".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "The file is not valid UTF-8 text.".to_string())
}

fn write_text_file_impl(path: &str, content: &str) -> Result<(), String> {
    let path = validate_editable_path(path, true)?;
    if content.len() as u64 > MAX_TEXT_FILE_BYTES {
        return Err("The file exceeds the 5 MiB editing limit.".to_string());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|error| format!("Unable to open the file for writing: {error}"))?;
    file.write_all(content.as_bytes())
        .map_err(|error| format!("Unable to write the file: {error}"))
}

#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    read_text_file_impl(&path)
}

#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), String> {
    write_text_file_impl(&path, &content)
}

pub fn compute_file_hashes_impl(
    path: &str,
) -> Result<gtavmm_core::hash_calculator::FileHashes, String> {
    gtavmm_core::hash_calculator::compute(std::path::Path::new(path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn compute_file_hashes(
    path: String,
) -> Result<gtavmm_core::hash_calculator::FileHashes, String> {
    compute_file_hashes_impl(&path)
}

// ---------------------------------------------------------------------------
// Malware scan (gtavmm_core::malware_scan) — shells out to whatever OS-native
// antivirus is already present; never a self-maintained scan engine.
// ---------------------------------------------------------------------------

pub fn scan_mod_path_impl(path: &str) -> Result<gtavmm_core::malware_scan::ScanOutcome, String> {
    gtavmm_core::malware_scan::scan_path(std::path::Path::new(path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn scan_mod_path(path: String) -> Result<gtavmm_core::malware_scan::ScanOutcome, String> {
    scan_mod_path_impl(&path)
}

// ---------------------------------------------------------------------------
// Update check (gtavmm_core::update_check) — checks GitHub Releases only when
// explicitly called; never runs on its own, per the project's offline-first
// default. Does not download or apply anything.
// ---------------------------------------------------------------------------

pub fn check_for_update_impl() -> Result<gtavmm_core::update_check::UpdateCheckResult, String> {
    gtavmm_core::update_check::check(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn check_for_update() -> Result<gtavmm_core::update_check::UpdateCheckResult, String> {
    check_for_update_impl()
}

// ---------------------------------------------------------------------------
// Mod source release check (gtavmm_core::mod_source_check) — manual metadata
// lookup only; it never downloads or installs a release.
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code
// ---------------------------------------------------------------------------

pub fn check_mod_source_release_impl(
    source_url: &str,
) -> Result<gtavmm_core::mod_source_check::ModSourceCheckResult, String> {
    gtavmm_core::mod_source_check::check_source(source_url).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn check_mod_source_release(
    source_url: String,
) -> Result<gtavmm_core::mod_source_check::ModSourceCheckResult, String> {
    check_mod_source_release_impl(&source_url)
}

// ---------------------------------------------------------------------------
// Mod library search (gtavmm_core::mod_search) — real, fully local, always-
// available keyword search (not natural-language understanding — see the
// module's own honesty note for why).
// ---------------------------------------------------------------------------

pub fn search_mods_impl(
    conn: &Connection,
    query: &str,
) -> Result<Vec<gtavmm_core::mod_search::ModSearchResult>, String> {
    gtavmm_core::mod_search::search_mods(conn, query).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_mods(
    state: tauri::State<crate::AppState>,
    query: String,
) -> Result<Vec<gtavmm_core::mod_search::ModSearchResult>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    search_mods_impl(&conn, &query)
}

// ---------------------------------------------------------------------------
// Mod lifecycle: disable, enable, uninstall, reinstall.
//
// The engine has done all four since early on and they carry twenty tests
// between them, but none was ever exposed to the interface, so the application
// could install a mod and then had no way to turn it off or remove it again.
// These are deliberately thin: each resolves the paths the core function needs
// and calls straight into it, adding no logic of its own. Every guard that
// matters — status checks, backup restoration, recycle bin snapshots, add-on
// pack deregistration — already lives in the core and is tested there.
// ---------------------------------------------------------------------------

pub fn disable_mod_impl(
    conn: &Connection,
    mod_id: i64,
    staging_root: &std::path::Path,
) -> Result<(), String> {
    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    let result = gtavmm_core::state::disable(conn, mod_id, staging_root);
    if let Err(error @ gtavmm_core::CoreError::ModDisablingUnavailable) = &result {
        return Err(error.to_string());
    }
    let result = result.map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ =
            gtavmm_core::app_log::error(&format!("disable_mod failed for id {mod_id}: {reason}"));
    }
    result
}

#[tauri::command]
pub fn disable_mod(state: tauri::State<crate::AppState>, mod_id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let staging_root = default_staging_root()?;
    disable_mod_impl(&conn, mod_id, &staging_root)
}

pub fn enable_mod_impl(
    conn: &Connection,
    mod_id: i64,
    staging_root: &std::path::Path,
) -> Result<(), String> {
    let result = gtavmm_core::state::enable(conn, mod_id, staging_root).map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ =
            gtavmm_core::app_log::error(&format!("enable_mod failed for id {mod_id}: {reason}"));
    }
    result
}

#[tauri::command]
pub fn enable_mod(state: tauri::State<crate::AppState>, mod_id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let staging_root = default_staging_root()?;
    enable_mod_impl(&conn, mod_id, &staging_root)
}

// OpenAI-Codex: Legacy wrappers remain available to unit tests only.
#[cfg(test)]
pub fn uninstall_mod_impl(
    conn: &mut Connection,
    mod_id: i64,
    game_path: Option<&str>,
    recycle_bin_root: &std::path::Path,
) -> Result<(), String> {
    let (game_root, _) =
        gtavmm_core::providers::resolve_game_root(game_path.map(std::path::Path::new))
            .map_err(|e| e.to_string())?;
    let result = gtavmm_core::uninstall::uninstall(conn, mod_id, &game_root, recycle_bin_root)
        .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ =
            gtavmm_core::app_log::error(&format!("uninstall_mod failed for id {mod_id}: {reason}"));
    }
    result
}

/// OpenAI-Codex: Resolve page-aware roots so FiveM Client paths bypass GTA V detection.
#[cfg(test)]
pub fn uninstall_mod_for_page_impl(
    conn: &mut Connection,
    mod_id: i64,
    game_path: Option<&str>,
    mode: &str,
    recycle_bin_root: &std::path::Path,
) -> Result<(), String> {
    let (game_root, _provider) = resolve_page_target(conn, game_path, mode)?;
    let result = gtavmm_core::uninstall::uninstall(conn, mod_id, &game_root, recycle_bin_root)
        .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ =
            gtavmm_core::app_log::error(&format!("uninstall_mod failed for id {mod_id}: {reason}"));
    }
    result
}

/// OpenAI-Codex: Explicit context keeps disabled snapshots in app-owned storage.
pub fn uninstall_mod_with_staging_impl(
    conn: &mut Connection,
    mod_id: i64,
    game_path: Option<&str>,
    mode: Option<&str>,
    recycle_bin_root: &std::path::Path,
    staging_root: &std::path::Path,
) -> Result<(), String> {
    let game_root = match mode {
        Some(mode) => resolve_page_target(conn, game_path, mode)?.0,
        None => {
            gtavmm_core::providers::resolve_game_root(game_path.map(std::path::Path::new))
                .map_err(|e| e.to_string())?
                .0
        }
    };
    let result = gtavmm_core::uninstall::uninstall_with_staging(
        conn,
        mod_id,
        &game_root,
        recycle_bin_root,
        staging_root,
    )
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ =
            gtavmm_core::app_log::error(&format!("uninstall_mod failed for id {mod_id}: {reason}"));
    }
    result
}

#[tauri::command]
pub fn uninstall_mod(
    state: tauri::State<crate::AppState>,
    mod_id: i64,
    game_path: Option<String>,
    mode: Option<String>,
) -> Result<(), String> {
    let mut conn = state.conn.lock().map_err(|e| e.to_string())?;
    let recycle_bin_root = default_recycle_bin_root()?;
    let staging_root = default_staging_root()?;
    uninstall_mod_with_staging_impl(
        &mut conn,
        mod_id,
        game_path.as_deref(),
        mode.as_deref(),
        &recycle_bin_root,
        &staging_root,
    )
}

/// Replaces a mod's files from a newer download. The caller supplies the source
/// path because this project never fetches a mod on the user's behalf.
///
/// Note the core deliberately does not roll back across the uninstall/install
/// boundary: if the install half fails, the mod is left uninstalled and
/// recoverable from the recycle bin rather than silently restored. A real
/// interruption surfaces as a real failure.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn reinstall_mod_impl(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &str,
    version_label: &str,
    mode: &str,
    game_path: Option<&str>,
    backup_root: &std::path::Path,
    recycle_bin_root: &std::path::Path,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    reinstall_mod_with_optional_staging_impl(
        conn,
        mod_id,
        new_source_path,
        version_label,
        mode,
        game_path,
        backup_root,
        recycle_bin_root,
        None,
        true,
    )
}

/// OpenAI-Codex: The caller supplies the same staging root used for disable.
#[allow(clippy::too_many_arguments)]
pub fn reinstall_mod_with_staging_impl(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &str,
    version_label: &str,
    mode: &str,
    game_path: Option<&str>,
    backup_root: &std::path::Path,
    recycle_bin_root: &std::path::Path,
    staging_root: &std::path::Path,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    reinstall_mod_with_optional_staging_impl(
        conn,
        mod_id,
        new_source_path,
        version_label,
        mode,
        game_path,
        backup_root,
        recycle_bin_root,
        Some(staging_root),
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn reinstall_mod_with_optional_staging_impl(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &str,
    version_label: &str,
    mode: &str,
    game_path: Option<&str>,
    backup_root: &std::path::Path,
    recycle_bin_root: &std::path::Path,
    staging_root: Option<&std::path::Path>,
    auto_backup: bool,
) -> Result<gtavmm_core::install::InstallOutcome, String> {
    let (game_root, provider) = resolve_page_target(conn, game_path, mode)?;
    // OpenAI-Codex_lspdfr_role_app: Recover the recorded destination before core classification/uninstall.
    let role = crate::install_role::infer_saved_role(conn, mod_id, &game_root)?;
    if let Some(role) = role {
        crate::install_role::classify(
            std::path::Path::new(new_source_path),
            mode,
            provider.as_ref(),
            Some(role.as_str()),
        )?;
    }
    let provider = crate::install_role::RoleProvider {
        inner: provider.as_ref(),
        role,
    };
    let options = gtavmm_core::install::InstallOptions {
        auto_backup,
        override_foreign_conflicts: false,
    };
    let result = if let Some(staging_root) = staging_root {
        gtavmm_core::install::reinstall_with_staging(
            conn,
            mod_id,
            std::path::Path::new(new_source_path),
            version_label,
            &provider,
            &game_root,
            backup_root,
            recycle_bin_root,
            options,
            staging_root,
        )
    } else {
        gtavmm_core::install::reinstall(
            conn,
            mod_id,
            std::path::Path::new(new_source_path),
            version_label,
            &provider,
            &game_root,
            backup_root,
            recycle_bin_root,
            options,
        )
    }
    .and_then(gtavmm_core::install::InstallOutcome::require_success)
    .map_err(|e| e.to_string());
    if let Err(reason) = &result {
        let _ =
            gtavmm_core::app_log::error(&format!("reinstall_mod failed for id {mod_id}: {reason}"));
    }
    result
}

#[tauri::command]
pub async fn reinstall_mod(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    mod_id: i64,
    new_source_path: String,
    version_label: String,
    mode: String,
    game_path: Option<String>,
) -> Result<gtavmm_core::install::InstallOutcome, BackupCommandError> {
    let (snapshot, target_before) = {
        let mut conn = state.conn.lock().map_err(|error| error.to_string())?;
        let settings = load_user_settings_impl(&conn)?;
        if settings.default_auto_backup {
            return run_reinstall_with_backup(
                &mut conn, mod_id, &new_source_path, &version_label, &mode,
                game_path.as_deref(), true,
            ).map_err(BackupCommandError::from);
        }
        let snapshot = backup_confirm_snapshot(&conn)?;
        if snapshot.settings != settings {
            return Err("Settings changed. Review and retry.".to_string().into());
        }
        let target = reinstall_confirmation_target(&conn, game_path.as_deref(), &mode, mod_id)?;
        (snapshot, target)
    };
    let (confirmed, deadline) = request_backup_warning(
        &app, &snapshot.settings.language,
        BackupWarning::Reinstall { id: mod_id, name: &target_before.name },
    ).await?;
    if !confirmed {
        return Err(backup_cancelled());
    }
    let mut conn = state.conn.lock().map_err(|error| error.to_string())?;
    ensure_backup_deadline(deadline, &snapshot.settings.language)?;
    let auto_backup = checked_backup_choice(&conn, &snapshot, confirmed)?;
    if reinstall_confirmation_target(&conn, game_path.as_deref(), &mode, mod_id)? != target_before {
        return Err("The reinstall target changed. Review and retry.".to_string().into());
    }
    ensure_backup_deadline(deadline, &snapshot.settings.language)?;
    run_reinstall_with_backup(
        &mut conn, mod_id, &new_source_path, &version_label, &mode,
        game_path.as_deref(), auto_backup,
    ).map_err(BackupCommandError::from)
}

// ---------------------------------------------------------------------------
// User settings.
//
// `settings::load`/`save` have existed and been tested since early on, but only
// language was ever reachable — the interface had no way to read or write the
// theme, the terms acceptance, the first-run state or the backup location, so
// none of those screens could exist. These expose the whole row.
// ---------------------------------------------------------------------------

/// The startup state the shell needs before it can decide what to show: the
/// terms gate, first-run setup, or the workspace.
#[derive(Debug, Clone, Serialize)]
pub struct StartupState {
    /// The user's choice — "system", "dark" or "light" — not what it resolves to.
    pub theme: String,
    pub terms_accepted: bool,
    pub onboarding_completed: bool,
    pub language: String,
}

pub fn load_startup_state_impl(conn: &Connection) -> Result<StartupState, String> {
    let s = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    Ok(StartupState {
        // An unset theme means the user never chose, which is "follow the OS".
        theme: s.theme.clone().unwrap_or_else(|| "system".to_string()),
        terms_accepted: gtavmm_core::settings::has_accepted_current_terms(&s),
        onboarding_completed: s.onboarding_completed,
        language: s.language.clone(),
    })
}

#[tauri::command]
pub fn load_startup_state(state: tauri::State<crate::AppState>) -> Result<StartupState, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    load_startup_state_impl(&conn)
}

pub fn set_theme_impl(conn: &Connection, theme: &str) -> Result<(), String> {
    if !matches!(theme, "system" | "dark" | "light") {
        return Err(format!(
            "unknown theme: {theme} (expected system/dark/light)"
        ));
    }
    let mut settings = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    settings.theme = Some(theme.to_string());
    gtavmm_core::settings::save(conn, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_theme(state: tauri::State<crate::AppState>, theme: String) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    set_theme_impl(&conn, &theme)
}

/// Records acceptance of the terms version the application is currently showing.
/// Storing the version rather than a flag is what lets a later revision ask
/// again instead of passing on stale consent.
pub fn accept_terms_impl(conn: &Connection) -> Result<(), String> {
    let mut settings = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    settings.terms_accepted_version =
        Some(gtavmm_core::settings::CURRENT_TERMS_VERSION.to_string());
    gtavmm_core::settings::save(conn, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn accept_terms(state: tauri::State<crate::AppState>) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    accept_terms_impl(&conn)
}

pub fn complete_onboarding_impl(
    conn: &Connection,
    game_install_path_override: Option<&str>,
) -> Result<(), String> {
    let mut settings = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    // Only overwrite the stored path when one was actually supplied; passing
    // nothing means "keep whatever detection or a previous run already set",
    // not "clear it".
    if let Some(path) = game_install_path_override {
        settings.game_install_path_override = Some(path.to_string());
    }
    settings.onboarding_completed = true;
    gtavmm_core::settings::save(conn, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn complete_onboarding(
    state: tauri::State<crate::AppState>,
    game_install_path_override: Option<String>,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    complete_onboarding_impl(&conn, game_install_path_override.as_deref())
}

/// The whole settings row, for the Backup and Game paths panels.
pub fn load_user_settings_impl(
    conn: &Connection,
) -> Result<gtavmm_core::db::models::UserSettings, String> {
    gtavmm_core::settings::load(conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_user_settings(
    state: tauri::State<crate::AppState>,
) -> Result<gtavmm_core::db::models::UserSettings, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    load_user_settings_impl(&conn)
}

pub fn load_fivem_client_path_impl(conn: &Connection) -> Result<Option<String>, String> {
    gtavmm_core::settings::load(conn)
        .map(|settings| settings.fivem_client_path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_fivem_client_path(
    state: tauri::State<crate::AppState>,
) -> Result<Option<String>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    load_fivem_client_path_impl(&conn)
}

pub fn save_fivem_client_path_impl(conn: &Connection, path: &str) -> Result<String, String> {
    gtavmm_core::providers::ensure_mode_available("fivem-client")
        .map_err(|error| error.to_string())?;
    let path = path.trim();
    if path.is_empty() {
        return Err("FiveM Client path cannot be empty".to_string());
    }
    if !std::path::Path::new(path).is_dir() {
        return Err(format!("{path} is not an existing directory"));
    }

    let mut settings = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    settings.fivem_client_path = Some(path.to_string());
    gtavmm_core::settings::save(conn, &settings).map_err(|e| e.to_string())?;
    Ok(path.to_string())
}

#[tauri::command]
pub fn save_fivem_client_path(
    state: tauri::State<crate::AppState>,
    path: String,
) -> Result<String, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    save_fivem_client_path_impl(&conn, &path)
}

/// How a panel wants one optional path changed.
///
/// Deliberately an explicit three-way rather than `Option<Option<String>>`:
/// serde reads a JSON `null` for a double option as the outer `None`, so
/// "clear this path" would arrive looking exactly like "leave it alone" and the
/// clear button would silently do nothing.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PathChange {
    /// Not mentioned by the caller — keep whatever is stored.
    #[default]
    Keep,
    /// Remove the stored value.
    Clear,
    /// Replace the stored value.
    Set { path: String },
}

impl PathChange {
    fn apply(self, target: &mut Option<String>) {
        match self {
            PathChange::Keep => {}
            PathChange::Clear => *target = None,
            PathChange::Set { path } => *target = Some(path),
        }
    }
}

/// Updates the settings the Backup and Game paths panels own. Deliberately not a
/// blanket "save this whole row": a panel that only shows two fields must not be
/// able to silently reset the terms acceptance or the onboarding state by
/// writing back a row it never fully loaded.
pub fn update_user_settings_impl(
    conn: &Connection,
    default_auto_backup: Option<bool>,
    game_install_path_override: PathChange,
    backup_root_override: PathChange,
) -> Result<gtavmm_core::db::models::UserSettings, String> {
    let mut settings = gtavmm_core::settings::load(conn).map_err(|e| e.to_string())?;
    if let Some(v) = default_auto_backup {
        settings.default_auto_backup = v;
    }
    game_install_path_override.apply(&mut settings.game_install_path_override);
    backup_root_override.apply(&mut settings.backup_root_override);
    gtavmm_core::settings::save(conn, &settings).map_err(|e| e.to_string())?;
    Ok(settings)
}

#[tauri::command]
pub async fn update_user_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    default_auto_backup: Option<bool>,
    game_install_path_override: Option<PathChange>,
    backup_root_override: Option<PathChange>,
) -> Result<gtavmm_core::db::models::UserSettings, BackupCommandError> {
    let snapshot = {
        let conn = state.conn.lock().map_err(|error| error.to_string())?;
        if default_auto_backup != Some(false) {
            return update_user_settings_impl(
                &conn, default_auto_backup,
                game_install_path_override.unwrap_or_default(),
                backup_root_override.unwrap_or_default(),
            ).map_err(BackupCommandError::from);
        }
        backup_confirm_snapshot(&conn)?
    };
    let (confirmed, deadline) =
        request_backup_warning(&app, &snapshot.settings.language, BackupWarning::Settings).await?;
    if !confirmed {
        return Err(backup_cancelled());
    }
    let conn = state.conn.lock().map_err(|error| error.to_string())?;
    ensure_backup_deadline(deadline, &snapshot.settings.language)?;
    recheck_backup_snapshot(&conn, &snapshot)?;
    ensure_backup_deadline(deadline, &snapshot.settings.language)?;
    update_user_settings_impl(
        &conn,
        default_auto_backup,
        game_install_path_override.unwrap_or_default(),
        backup_root_override.unwrap_or_default(),
    )
    .map_err(BackupCommandError::from)
}

/// Checks a manually chosen game folder before it is saved, so a wrong path is
/// rejected at the point of choosing rather than at the next install.
pub fn validate_game_path_impl(path: &str) -> Result<DetectGameResult, String> {
    use gtavmm_core::game_locator::DetectResult;
    match gtavmm_core::game_locator::validate_manual_path(std::path::Path::new(path)) {
        Ok(DetectResult::Found(installation)) => Ok(DetectGameResult::Found {
            install_path: installation.install_path,
            edition: installation.edition,
        }),
        Ok(DetectResult::FoundUnsupportedEdition { .. }) | Ok(DetectResult::NotFound) => {
            Ok(DetectGameResult::NotFound)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn validate_game_path(path: String) -> Result<DetectGameResult, String> {
    validate_game_path_impl(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // OpenAI-Codex_backup_flow: Compare same-connection changes and fail closed.
    #[test]
    fn backup_confirm_snapshot_detects_same_connection_aba() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let before = backup_confirm_snapshot(&conn).unwrap();
        let mut changed = load_user_settings_impl(&conn).unwrap();
        let original = changed.clone();
        changed.default_auto_backup = !changed.default_auto_backup;
        gtavmm_core::settings::save(&conn, &changed).unwrap();
        gtavmm_core::settings::save(&conn, &original).unwrap();
        assert_eq!(load_user_settings_impl(&conn).unwrap(), before.settings);
        assert_ne!(backup_confirm_snapshot(&conn).unwrap(), before);
    }

    #[test]
    fn backup_confirm_snapshot_rejects_missing_settings() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(backup_confirm_snapshot(&conn).is_err());
    }

    #[test]
    fn backup_choice_rejects_declined_and_stale_confirmation() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let mut settings = load_user_settings_impl(&conn).unwrap();
        settings.default_auto_backup = false;
        gtavmm_core::settings::save(&conn, &settings).unwrap();
        let before = backup_confirm_snapshot(&conn).unwrap();
        assert!(matches!(
            checked_backup_choice(&conn, &before, false),
            Err(BackupCommandError::Cancelled { .. })
        ));
        assert_eq!(backup_confirm_snapshot(&conn).unwrap(), before);
        settings.default_auto_backup = true;
        gtavmm_core::settings::save(&conn, &settings).unwrap();
        assert!(matches!(
            checked_backup_choice(&conn, &before, true),
            Err(BackupCommandError::Failure(_))
        ));
    }

    #[test]
    fn backup_command_errors_preserve_old_string_shape() {
        assert_eq!(
            serde_json::to_value(BackupCommandError::Failure("error".to_string())).unwrap(),
            serde_json::json!("error")
        );
        assert_eq!(
            serde_json::to_value(backup_cancelled()).unwrap(),
            serde_json::json!({"code": "backup_confirmation_cancelled"})
        );
    }

    // OpenAI-Codex_backup_flow: The native callback owns the lease even after timeout.
    // OpenAI-Codex_backup_flow: A callback alone cannot release a still-running show task.
    #[test]
    fn callback_completion_keeps_lease_until_show_task_finishes() {
        static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let callback_guard = acquire_backup_warning(&ACTIVE).unwrap();
        let task_guard = std::sync::Arc::clone(&callback_guard);
        let (sender, _receiver) = std::sync::mpsc::channel();
        let callback = backup_warning_callback(sender, callback_guard);
        callback(true);
        assert!(acquire_backup_warning(&ACTIVE).is_none());
        drop(task_guard);
        assert!(acquire_backup_warning(&ACTIVE).is_some());
    }

    #[test]
    fn dropped_callback_keeps_lease_while_show_task_is_pending() {
        static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let callback_guard = acquire_backup_warning(&ACTIVE).unwrap();
        let task_guard = std::sync::Arc::clone(&callback_guard);
        let (sender, _receiver) = std::sync::mpsc::channel();
        drop(backup_warning_callback(sender, callback_guard));
        assert!(acquire_backup_warning(&ACTIVE).is_none());
        drop(task_guard);
        assert!(acquire_backup_warning(&ACTIVE).is_some());
    }

    #[test]
    fn expired_confirmation_deadline_fails_before_dispatch() {
        let error = ensure_backup_deadline(std::time::Instant::now(), "en").unwrap_err();
        assert!(error.contains("120 seconds"));
    }

    #[test]
    fn expired_confirmation_discards_late_yes_but_keeps_guard_until_callback() {
        static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let guard = acquire_backup_warning(&ACTIVE).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let callback = backup_warning_callback(sender, guard);
        let pending = PendingBackupWarning { receiver, deadline: std::time::Instant::now() };
        assert_eq!(receive_backup_warning(pending), Err(BackupWarningWaitError::TimedOut));
        assert!(acquire_backup_warning(&ACTIVE).is_none());
        callback(true);
        assert!(acquire_backup_warning(&ACTIVE).is_some());
    }

    #[test]
    fn dropping_confirmation_callback_releases_guard_and_disconnects() {
        static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let guard = acquire_backup_warning(&ACTIVE).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let callback = backup_warning_callback(sender, guard);
        drop(callback);
        let pending = PendingBackupWarning {
            receiver,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        };
        assert_eq!(receive_backup_warning(pending), Err(BackupWarningWaitError::Disconnected));
        assert!(acquire_backup_warning(&ACTIVE).is_some());
    }

    #[test]
    fn queued_confirmation_after_deadline_is_rejected() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(true).unwrap();
        let pending = PendingBackupWarning { receiver, deadline: std::time::Instant::now() };
        assert_eq!(receive_backup_warning(pending), Err(BackupWarningWaitError::TimedOut));
    }

    #[test]
    fn confirmation_response_before_deadline_is_observed() {
        static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let guard = acquire_backup_warning(&ACTIVE).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let callback = backup_warning_callback(sender, guard);
        callback(true);
        let pending = PendingBackupWarning {
            receiver,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        };
        assert_eq!(receive_backup_warning(pending), Ok(true));
        assert!(acquire_backup_warning(&ACTIVE).is_some());
    }

    #[test]
    fn warning_label_omits_path_and_filters_bidi_and_controls() {
        let label = backup_warning_label("C:\\private\\ab\u{202e}cd\n.ini");
        assert_eq!(label, "abcd.ini");
        let input = format!("C:\\private\\{}", "a".repeat(100));
        let short = backup_warning_label(&input);
        assert_eq!(short.chars().count(), 81);
        assert!(short.ends_with('…'));
        assert!(!short.contains("private"));
    }



    // OpenAI-Codex: Draft preparation never reads credentials or makes a request.
    #[test]
    fn dlc_ai_preview_rejects_changed_inputs_and_settings_without_transport() {
        let xml = "<SMandatoryPacksData><Paths/></SMandatoryPacksData>";
        let settings = gtavmm_core::ai_assistant::AiSettings {
            enabled: true,
            provider: Some(gtavmm_core::ai_assistant::AiProviderKind::Ollama),
            ..Default::default()
        };
        let preview = prepare_dlclist_ai_impl(&settings, xml, "Add a pack").unwrap();
        assert!(check_dlclist_ai_preview(&settings, xml, "Add a pack", &preview).is_ok());
        assert!(check_dlclist_ai_preview(&settings, xml, "Remove a pack", &preview).is_err());
        let mut changed = settings.clone();
        changed.ollama_model = Some("different-model".into());
        assert!(check_dlclist_ai_preview(&changed, xml, "Add a pack", &preview).is_err());
        changed.enabled = false;
        assert!(prepare_dlclist_ai_impl(&changed, xml, "Add a pack").is_err());
        assert!(prepare_dlclist_ai_impl(&settings, "invalid", "Add a pack").is_err());
        assert!(!preview.user_prompt.contains("source_path"));
    }

    #[test]
    fn dlc_ai_result_checks_protected_content_before_returning_analysis() {
        let xml = "<SMandatoryPacksData flag=\"keep\"><Paths/></SMandatoryPacksData>";
        assert!(dlclist_ai_result(xml, xml.replace("keep", "changed")).is_err());
        let result = dlclist_ai_result(
            xml,
            xml.replace("<Paths/>", "<Paths><Item>dlcpacks:/demo/</Item></Paths>"),
        )
        .unwrap();
        assert_eq!(result.analysis.diff.added, vec!["dlcpacks:/demo/"]);
    }

    fn create_file_symlink(
        target: &std::path::Path,
        link: &std::path::Path,
    ) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
    }

    fn create_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
    }

    #[test]
    fn validate_editable_path_rejects_empty_relative_unsupported_and_protected_paths() {
        assert!(validate_editable_path("", true).is_err());
        assert!(validate_editable_path("relative.ini", true).is_err());
        assert!(validate_editable_path("C:\\mods\\tool.exe", true).is_err());
        assert!(validate_editable_path("C:\\mods\\README", true).is_err());

        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("GTA5.exe");
        std::fs::write(&protected, b"protected").unwrap();
        let error = validate_editable_path(protected.to_str().unwrap(), true).unwrap_err();
        assert!(error.to_ascii_lowercase().contains("protected"));
    }

    #[test]
    fn validate_editable_path_accepts_allowlisted_extensions_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        for extension in ["ini", "XML", "json", "META", "cfg", "TXT"] {
            let path = dir.path().join(format!("settings.{extension}"));
            std::fs::write(&path, b"ok").unwrap();
            assert!(validate_editable_path(path.to_str().unwrap(), true).is_ok());
        }
    }

    #[test]
    fn validate_editable_path_rejects_symlinked_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target-dir");
        let linked_dir = dir.path().join("linked-dir");
        std::fs::create_dir(&target_dir).unwrap();
        let target_file = target_dir.join("settings.ini");
        std::fs::write(&target_file, "original").unwrap();

        if let Err(error) = create_dir_symlink(&target_dir, &linked_dir) {
            if cfg!(windows) && error.raw_os_error() == Some(1314) {
                eprintln!("SKIPPED parent-symlink assertions: this Windows account cannot create directory symlinks (error 1314).");
                return;
            }
            panic!("failed to create test directory symlink: {error}");
        }

        let linked_file = linked_dir.join("settings.ini");
        assert!(validate_editable_path(linked_file.to_str().unwrap(), true).is_err());
        assert!(read_text_file_impl(linked_file.to_str().unwrap()).is_err());
        assert!(write_text_file_impl(linked_file.to_str().unwrap(), "changed").is_err());
        assert_eq!(std::fs::read_to_string(target_file).unwrap(), "original");
    }

    #[test]
    fn read_text_file_impl_rejects_invalid_paths_and_files_over_five_mib() {
        assert!(read_text_file_impl("").is_err());
        assert!(read_text_file_impl("relative.ini").is_err());

        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("unsafe.exe");
        std::fs::write(&executable, b"not editable").unwrap();
        assert!(read_text_file_impl(executable.to_str().unwrap()).is_err());

        let protected = dir.path().join("GTA5.exe");
        std::fs::write(&protected, b"protected").unwrap();
        assert!(read_text_file_impl(protected.to_str().unwrap())
            .unwrap_err()
            .to_ascii_lowercase()
            .contains("protected"));

        let oversized = dir.path().join("large.txt");
        std::fs::write(&oversized, vec![b'x'; 5 * 1024 * 1024 + 1]).unwrap();
        assert!(read_text_file_impl(oversized.to_str().unwrap()).is_err());

        let text = dir.path().join("notes.TXT");
        std::fs::write(&text, "editable text").unwrap();
        assert_eq!(
            read_text_file_impl(text.to_str().unwrap()).unwrap(),
            "editable text"
        );
    }

    #[test]
    fn write_text_file_impl_only_updates_safe_existing_files_within_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let editable = dir.path().join("settings.InI");
        std::fs::write(&editable, "before").unwrap();
        write_text_file_impl(editable.to_str().unwrap(), "after").unwrap();
        assert_eq!(std::fs::read_to_string(&editable).unwrap(), "after");

        let missing = dir.path().join("new.ini");
        assert!(write_text_file_impl(missing.to_str().unwrap(), "new").is_err());
        assert!(!missing.exists());

        let executable = dir.path().join("unsafe.exe");
        std::fs::write(&executable, "keep").unwrap();
        assert!(write_text_file_impl(executable.to_str().unwrap(), "replace").is_err());
        assert_eq!(std::fs::read_to_string(executable).unwrap(), "keep");

        let oversized = dir.path().join("large.txt");
        std::fs::write(&oversized, "small").unwrap();
        assert!(write_text_file_impl(
            oversized.to_str().unwrap(),
            &"x".repeat(5 * 1024 * 1024 + 1)
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(oversized).unwrap(), "small");

        let target = dir.path().join("target.ini");
        let link = dir.path().join("link.ini");
        std::fs::write(&target, "target").unwrap();
        if let Err(error) = create_file_symlink(&target, &link) {
            if cfg!(windows) && error.raw_os_error() == Some(1314) {
                eprintln!("SKIPPED symlink assertions: this Windows account cannot create file symlinks (error 1314).");
                return;
            }
            panic!("failed to create test file symlink: {error}");
        }
        assert!(validate_editable_path(link.to_str().unwrap(), true).is_err());
        assert!(read_text_file_impl(link.to_str().unwrap()).is_err());
        assert!(write_text_file_impl(link.to_str().unwrap(), "changed").is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "target");
    }

    #[test]
    fn unsupported_mod_source_release_lookup_does_not_query_github() {
        assert_eq!(
            check_mod_source_release_impl("https://mods.example/item/1"),
            Ok(gtavmm_core::mod_source_check::ModSourceCheckResult::UnsupportedSource)
        );
    }

    fn write_manifest(dir: &std::path::Path, name: &str, body: &str) {
        let resource_dir = dir.join(name);
        std::fs::create_dir_all(&resource_dir).unwrap();
        std::fs::write(resource_dir.join("fxmanifest.lua"), body).unwrap();
    }

    /// A fake game install directory that `game_locator::classify_edition` will
    /// recognize as Legacy — enough for `providers::resolve` to succeed against it.
    fn fake_game_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("GTA5.exe"), b"").unwrap();
        dir
    }

    // OpenAI-Codex_install_preflight_backend: Synthetic preflight contracts; not runtime acceptance.
    fn preflight_fixture(
        conn: &Connection,
        root: &std::path::Path,
        mode: &str,
        source: &std::path::Path,
    ) -> Result<InstallPreview, String> {
        ensure_install_preview_mode(mode)?;
        let (root, _) = resolve_page_target(conn, root.to_str(), mode)?;
        let plan = classify_install_preview(root.clone(), mode, source)?;
        finish_install_preview(conn, mode, &root, plan)
    }

    // OpenAI-Codex_lspdfr_role_app: Removing the role wrapper would break these destination contracts.
    #[test]
    fn callout_preview_install_and_reinstall_keep_the_same_destination() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = fake_game_root();
        let work = tempfile::tempdir().unwrap();
        let dll = work.path().join("Example.dll");
        let mut bytes = vec![0u8; 512];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[88..90].copy_from_slice(&0x10bu16.to_le_bytes());
        bytes[296..300].copy_from_slice(&1u32.to_le_bytes());
        bytes[300..304].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&dll, &bytes).unwrap();
        let provider = gtavmm_core::providers::LegacyLspdfrProvider::new(root.path().to_path_buf());
        let (plan, role, required) =
            crate::install_role::classify(&dll, "legacy-lspdfr", &provider, Some("callout"))
                .unwrap();
        let preview = finish_install_preview_with_role(
            &conn,
            "legacy-lspdfr",
            root.path(),
            plan,
            role,
            required,
        )
        .unwrap();
        assert!(preview.role_required);
        assert_eq!(
            preview.role,
            Some(crate::install_role::ManagedDllRole::Callout)
        );
        let target = preview.plan.files[0].target.clone();
        let installed = install_mod_with_role_impl(
            &mut conn,
            root.path().to_str(),
            "legacy-lspdfr",
            dll.to_str().unwrap(),
            None,
            false,
            &work.path().join("backup"),
            Some("callout"),
        )
        .unwrap();
        let gtavmm_core::install::InstallOutcome::Success {
            installed_mod_id, ..
        } = installed
        else {
            panic!("expected successful role installation");
        };
        assert_eq!(target, root.path().join("Plugins/LSPDFR/Example.dll"));
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert!(reinstall_mod_impl(
            &mut conn,
            installed_mod_id,
            dll.to_str().unwrap(),
            "wrong-page",
            "legacy-sp",
            root.path().to_str(),
            &work.path().join("wrong-backup"),
            &work.path().join("wrong-recycle"),
        )
        .is_err());
        assert!(target.is_file());
        assert!(!work.path().join("wrong-backup").exists());
        assert!(!work.path().join("wrong-recycle").exists());
        let outcome = reinstall_mod_impl(
            &mut conn,
            installed_mod_id,
            dll.to_str().unwrap(),
            "v2",
            "legacy-lspdfr",
            root.path().to_str(),
            &work.path().join("backup2"),
            &work.path().join("recycle"),
        )
        .unwrap();
        assert!(matches!(
            outcome,
            gtavmm_core::install::InstallOutcome::Success { .. }
        ));
        assert!(target.is_file());
        assert!(!root.path().join("Plugins/Example.dll").exists());
        let gtavmm_core::install::InstallOutcome::Success {
            installed_mod_id, ..
        } = outcome
        else {
            panic!("expected successful role reinstall");
        };
        let enabling_staging = tempfile::tempdir().unwrap();
        seed_synthetic_single_file_disabled(&conn, installed_mod_id, &root, &enabling_staging);
        assert!(!target.exists());
        enable_mod_impl(&conn, installed_mod_id, enabling_staging.path()).unwrap();
        assert!(target.is_file());
        let reinstall_staging = tempfile::tempdir().unwrap();
        seed_synthetic_single_file_disabled(&conn, installed_mod_id, &root, &reinstall_staging);
        reinstall_mod_with_staging_impl(
            &mut conn,
            installed_mod_id,
            dll.to_str().unwrap(),
            "v3",
            "legacy-lspdfr",
            root.path().to_str(),
            &work.path().join("backup3"),
            &work.path().join("recycle3"),
            reinstall_staging.path(),
        )
        .unwrap();
        assert!(target.is_file());
        assert!(!root.path().join("Plugins/Example.dll").exists());
    }

    #[test]
    fn role_install_rejects_wrong_mode_and_changed_preview_root_before_writes() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = fake_game_root();
        let work = tempfile::tempdir().unwrap();
        let backup = work.path().join("backup");
        let err = install_mod_with_role_impl(
            &mut conn,
            root.path().to_str(),
            "legacy-sp",
            "missing.dll",
            None,
            false,
            &backup,
            Some("callout"),
        )
        .unwrap_err();
        assert!(err.contains("standalone managed DLL"));
        assert!(!backup.exists());
        assert!(check_install_preview_root(
            &conn,
            root.path().to_str(),
            "legacy-lspdfr",
            Some("changed-root")
        )
        .is_err());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM installed_mod", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn install_preview_keeps_managed_dll_routes_separate_and_does_not_write_game_or_db() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = fake_game_root();
        let source = tempfile::tempdir().unwrap();
        let dll = source.path().join("Example.dll");
        let mut bytes = vec![0u8; 512];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[88..90].copy_from_slice(&0x10bu16.to_le_bytes());
        bytes[296..300].copy_from_slice(&1u32.to_le_bytes());
        bytes[300..304].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&dll, bytes).unwrap();
        let changes: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        for (mode, folder) in [("legacy-sp", "scripts"), ("legacy-lspdfr", "Plugins")] {
            let preview = preflight_fixture(&conn, root.path(), mode, &dll).unwrap();
            assert_eq!(preview.mode, mode);
            assert_eq!(
                preview.plan.files[0].target,
                root.path().join(folder).join("Example.dll")
            );
            assert_eq!(
                preview.plan.format,
                gtavmm_core::mod_analyzer::ModFormat::ManagedDll
            );
            assert!(preview.conflicts.foreign_conflicts.is_empty());
            assert!(!root.path().join(folder).exists());
        }
        assert_eq!(
            conn.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            changes
        );
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn install_preview_reports_protected_cross_page_owner_and_untracked_targets() {
        use gtavmm_core::mod_analyzer::{ModFormat, ModPlan, PlannedFile};
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = fake_game_root();
        let owned = root.path().join("Owned.asi");
        let unknown = root.path().join("Unknown.asi");
        std::fs::write(&unknown, b"untracked").unwrap();
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('SP owner', 'asi', ?1, 'active', 'legacy-sp')", [owned.to_string_lossy().as_ref()]).unwrap();
        let owner = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![owner, owned.to_string_lossy().as_ref()]).unwrap();
        let plan = ModPlan::new(
            ModFormat::FolderReplacer,
            [root.path().join("GTA5.exe"), owned, unknown]
                .into_iter()
                .map(|target| PlannedFile {
                    source: target.clone(),
                    target,
                })
                .collect(),
        );
        let preview = finish_install_preview(&conn, "legacy-lspdfr", root.path(), plan).unwrap();
        assert_eq!(preview.conflicts.protected_hits.len(), 1);
        assert_eq!(preview.conflicts.foreign_conflicts.len(), 2);
        assert!(preview
            .conflicts
            .foreign_conflicts
            .iter()
            .any(|c| c.owner_mod_id == Some(owner)));
        assert!(preview
            .conflicts
            .foreign_conflicts
            .iter()
            .any(|c| c.owner_mod_id.is_none()));
        assert!(preview.conflicts.self_update.is_some());
    }

    #[test]
    fn install_preview_rejects_pending_modes_before_missing_path_and_unsupported_plans() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        for mode in ["enhanced-sp", "enhanced-lspdfr", "fivem-client"] {
            let error = preflight_fixture(
                &conn,
                std::path::Path::new("missing-game"),
                mode,
                std::path::Path::new("missing-package.asi"),
            )
            .unwrap_err();
            assert!(error.contains("under development"), "{error}");
        }
        assert!(ensure_install_preview_mode("sp").is_err());
        assert!(ensure_install_preview_mode("lspdfr").is_err());
        let root = fake_game_root();
        let error = preflight_fixture(
            &conn,
            root.path(),
            "legacy-sp",
            std::path::Path::new("unsupported.oiv"),
        )
        .unwrap_err();
        assert!(error.contains("Unsupported"), "{error}");
        let empty = gtavmm_core::mod_analyzer::ModPlan::new(
            gtavmm_core::mod_analyzer::ModFormat::FolderReplacer,
            vec![],
        );
        assert!(
            finish_install_preview(&conn, "legacy-sp", root.path(), empty)
                .unwrap_err()
                .contains("no installable files")
        );
    }

    #[test]
    fn install_root_guard_rejects_changed_settings_and_preserves_legacy_callers() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let old = fake_game_root();
        let new = fake_game_root();
        complete_onboarding_impl(&conn, old.path().to_str()).unwrap();
        let (resolved, _) = resolve_page_target(&conn, None, "legacy-sp").unwrap();
        let expected = preview_root_identity(&resolved).unwrap();
        assert!(check_install_preview_root(&conn, None, "legacy-sp", Some(&expected)).is_ok());
        complete_onboarding_impl(&conn, new.path().to_str()).unwrap();
        assert!(
            check_install_preview_root(&conn, None, "legacy-sp", Some(&expected))
                .unwrap_err()
                .contains("preview again")
        );
        assert!(check_install_preview_root(&conn, None, "legacy-sp", None).is_ok());
    }

    #[test]
    fn install_rechecks_conflicts_introduced_after_preview() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = fake_game_root();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("Later.asi");
        std::fs::write(&file, b"new mod").unwrap();
        let preview = preflight_fixture(&conn, root.path(), "legacy-sp", &file).unwrap();
        assert!(preview.conflicts.foreign_conflicts.is_empty());
        let target = root.path().join("Later.asi");
        std::fs::write(&target, b"added after preview").unwrap();
        let backup = source.path().join("backups");
        let result = install_mod_impl(
            &mut conn,
            root.path().to_str(),
            "legacy-sp",
            file.to_str().unwrap(),
            None,
            false,
            &backup,
        )
        .unwrap();
        assert!(matches!(
            result,
            gtavmm_core::install::InstallOutcome::RequiresOverride(_)
        ));
        assert_eq!(std::fs::read(target).unwrap(), b"added after preview");
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    // OpenAI-Codex_install_preflight_backend: Missing targets must retain the DB path spelling on Windows.
    #[cfg(windows)]
    #[test]
    fn install_preview_retains_missing_target_owner_across_legacy_pages() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = fake_game_root();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("MissingOwned.asi");
        std::fs::write(&file, b"synthetic mod").unwrap();
        let target = root.path().join("MissingOwned.asi");
        assert!(!target.exists());
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('SP owner', 'asi', ?1, 'active', 'legacy-sp')", [target.to_string_lossy().as_ref()]).unwrap();
        let owner = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![owner, target.to_string_lossy().as_ref()]).unwrap();
        let preview = preflight_fixture(&conn, root.path(), "legacy-lspdfr", &file).unwrap();
        assert_eq!(preview.plan.files[0].target, target);
        assert_eq!(preview.conflicts.foreign_conflicts.len(), 1);
        assert_eq!(
            preview.conflicts.foreign_conflicts[0].owner_mod_id,
            Some(owner)
        );
        assert!(!target.exists());
    }

    // OpenAI-Codex_atomic_review: Export is exclusive and leaves the source unchanged.
    #[test]
    fn dlclist_copy_refuses_changed_source_existing_output_and_invalid_draft() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.xml");
        let output = dir.path().join("copy.xml");
        let original = "<SMandatoryPacksData><Paths></Paths></SMandatoryPacksData>";
        std::fs::write(&source, original).unwrap();
        let doc = read_dlclist_document_impl(source.to_str().unwrap()).unwrap();
        assert!(save_dlclist_copy_impl(
            source.to_str().unwrap(),
            &doc.source_hash,
            output.to_str().unwrap(),
            "<broken>"
        )
        .is_err());
        assert!(!output.exists());
        std::fs::write(&output, b"existing").unwrap();
        assert!(save_dlclist_copy_impl(
            source.to_str().unwrap(),
            &doc.source_hash,
            output.to_str().unwrap(),
            original
        )
        .is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"existing");
        std::fs::write(&source, format!("{original} ")).unwrap();
        let other = dir.path().join("other.xml");
        assert!(save_dlclist_copy_impl(
            source.to_str().unwrap(),
            &doc.source_hash,
            other.to_str().unwrap(),
            original
        )
        .is_err());
        assert!(!other.exists());
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            format!("{original} ")
        );
    }

    #[test]
    fn dlclist_copy_creates_new_file_without_changing_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.xml");
        let output = dir.path().join("copy.xml");
        let original = "<SMandatoryPacksData><Paths></Paths></SMandatoryPacksData>";
        std::fs::write(&source, original).unwrap();
        let doc = read_dlclist_document_impl(source.to_str().unwrap()).unwrap();
        save_dlclist_copy_impl(
            source.to_str().unwrap(),
            &doc.source_hash,
            output.to_str().unwrap(),
            original,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(source).unwrap(), original);
        assert_eq!(std::fs::read_to_string(output).unwrap(), original);
    }

    #[test]
    fn dlclist_read_rejects_oversized_input() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("large.xml");
        std::fs::write(
            &source,
            vec![b'x'; gtavmm_core::dlclist_editor::MAX_DLCLIST_BYTES + 1],
        )
        .unwrap();
        assert!(read_dlclist_document_impl(source.to_str().unwrap()).is_err());
    }

    #[test]
    fn dlclist_copy_rejects_game_override_paths() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.xml");
        let original = "<SMandatoryPacksData><Paths></Paths></SMandatoryPacksData>";
        std::fs::write(&source, original).unwrap();
        let doc = read_dlclist_document_impl(source.to_str().unwrap()).unwrap();
        for prefix in [
            "update/update.rpf/common/data",
            "mods/update/update.rpf/common/data",
        ] {
            let parent = dir.path().join(prefix);
            std::fs::create_dir_all(&parent).unwrap();
            let output = parent.join("dlclist.xml");
            assert!(save_dlclist_copy_impl(
                source.to_str().unwrap(),
                &doc.source_hash,
                output.to_str().unwrap(),
                original
            )
            .is_err());
            assert!(!output.exists());
        }
        assert_eq!(std::fs::read_to_string(source).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn dlclist_editor_rejects_source_and_output_data_streams() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.xml");
        let original = "<SMandatoryPacksData><Paths/></SMandatoryPacksData>";
        std::fs::write(&source, original).unwrap();
        let stream = format!("{}:copy.xml", source.display());
        assert!(dlclist_path_checks(&stream, true).is_err());
        assert!(dlclist_path_checks(&stream, false).is_err());
        let doc = read_dlclist_document_impl(source.to_str().unwrap()).unwrap();
        assert!(save_dlclist_copy_impl(
            source.to_str().unwrap(),
            &doc.source_hash,
            &stream,
            original
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn dlclist_copy_rejects_linked_output_parent() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.xml");
        let linked = dir.path().join("linked");
        let original = "<SMandatoryPacksData><Paths></Paths></SMandatoryPacksData>";
        std::fs::write(&source, original).unwrap();
        std::os::unix::fs::symlink(dir.path(), &linked).unwrap();
        let doc = read_dlclist_document_impl(source.to_str().unwrap()).unwrap();
        assert!(save_dlclist_copy_impl(
            source.to_str().unwrap(),
            &doc.source_hash,
            linked.join("copy.xml").to_str().unwrap(),
            original
        )
        .is_err());
        assert!(!dir.path().join("copy.xml").exists());
    }

    // OpenAI-Codex_atomic_review: Both Legacy pages share the same physical DLC work.
    #[test]
    fn manual_dlc_tasks_share_root_and_reject_unavailable_modes_or_query_errors() {
        let root = fake_game_root();
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        gtavmm_core::dlc_manual::begin_tasks(
            &mut conn,
            root.path(),
            &["ManualCar".to_string()],
            gtavmm_core::dlc_manual::ManualDlcOperation::Install,
            None,
        )
        .unwrap();
        let path = Some(root.path().to_str().unwrap());
        let sp = list_manual_dlc_tasks_impl(&conn, path, "legacy-sp").unwrap();
        let lspdfr = list_manual_dlc_tasks_impl(&conn, path, "legacy-lspdfr").unwrap();
        assert_eq!(sp.len(), 1);
        assert_eq!(sp[0].id, lspdfr[0].id);
        assert!(sp[0].unresolved);
        for mode in ["fivem-client", "enhanced-sp", "enhanced-lspdfr", "unknown"] {
            assert!(list_manual_dlc_tasks_impl(&conn, path, mode).is_err());
        }
        let empty = Connection::open_in_memory().unwrap();
        assert!(list_manual_dlc_tasks_impl(&empty, path, "legacy-sp").is_err());
    }

    // OpenAI-Codex: Keep a distinct marker for edition-mismatch regression cases.
    fn fake_enhanced_game_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("GTA5_Enhanced.exe"), b"").unwrap();
        dir
    }

    #[test]
    fn inspect_mod_impl_classifies_a_real_asi_file() {
        let game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();

        let plan = inspect_mod_impl(
            Some(game_dir.path().to_str().unwrap()),
            "legacy-sp",
            asi_path.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(plan.format, gtavmm_core::mod_analyzer::ModFormat::Asi);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].target, game_dir.path().join("SomeMod.asi"));
    }

    #[test]
    fn inspect_mod_impl_rejects_an_unknown_mode() {
        let game_dir = fake_game_root();
        let err = inspect_mod_impl(
            Some(game_dir.path().to_str().unwrap()),
            "not-a-mode",
            "x.asi",
        )
        .unwrap_err();
        assert!(err.contains("unknown mode"));
    }

    #[test]
    fn inspect_mod_for_settings_uses_the_saved_game_path_override() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let plan =
            inspect_mod_for_settings_impl(&conn, None, "legacy-sp", asi_path.to_str().unwrap())
                .unwrap();

        assert_eq!(
            plan.files[0].target,
            saved_game_dir.path().join("SomeMod.asi")
        );
    }

    #[test]
    fn page_target_uses_the_saved_game_path_when_no_path_is_passed() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let (game_root, _provider) = resolve_page_target(&conn, None, "legacy-sp").unwrap();

        assert_eq!(game_root, saved_game_dir.path().to_path_buf());
    }

    #[test]
    fn page_target_rejects_a_saved_path_for_the_wrong_edition() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let enhanced_game_dir = fake_enhanced_game_root();
        complete_onboarding_impl(&conn, enhanced_game_dir.path().to_str()).unwrap();

        let err = match resolve_page_target(&conn, None, "legacy-lspdfr") {
            Err(error) => error,
            Ok(_) => panic!("a Legacy LSPDFR page must reject an Enhanced install"),
        };

        assert!(err.contains("under development"), "{err}");
    }

    #[test]
    fn page_target_rejects_bare_provider_aliases_that_have_no_edition() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let err = match resolve_page_target(&conn, None, "sp") {
            Err(error) => error,
            Ok(_) => panic!("page-specific actions must require an edition-bearing page mode"),
        };

        assert!(err.contains("unknown page mode"), "{err}");
    }

    #[test]
    fn full_backup_edition_requires_a_gta_page() {
        assert_eq!(
            full_backup_edition_for_mode("legacy-sp").unwrap(),
            gtavmm_core::full_backup::GameEdition::Legacy
        );
        assert!(full_backup_edition_for_mode("enhanced-lspdfr").is_err());
        assert!(full_backup_edition_for_mode("fivem-client").is_err());
        assert!(full_backup_edition_for_mode("sp").is_err());
    }

    #[test]
    fn inspect_mod_for_settings_prefers_an_explicit_game_path_over_the_saved_override() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let explicit_game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let plan = inspect_mod_for_settings_impl(
            &conn,
            explicit_game_dir.path().to_str(),
            "legacy-sp",
            asi_path.to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(
            plan.files[0].target,
            explicit_game_dir.path().join("SomeMod.asi")
        );
    }

    #[test]
    fn inspect_mod_for_settings_preserves_bare_provider_mode_aliases() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let plan =
            inspect_mod_for_settings_impl(&conn, None, "sp", asi_path.to_str().unwrap()).unwrap();

        assert_eq!(
            plan.files[0].target,
            saved_game_dir.path().join("SomeMod.asi")
        );
    }

    // OpenAI-Codex: Cover LSPDFR alias, FiveM explicit roots, and preview lock release.
    #[test]
    fn mode_from_str_preserves_the_bare_lspdfr_provider_alias() {
        assert_eq!(
            mode_from_str("lspdfr").unwrap(),
            gtavmm_core::providers::Mode::Lspdfr
        );
    }

    #[test]
    fn xlsx_export_command_rejects_pages_other_than_legacy_sp() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();

        let err = export_mods_to_xlsx_impl(&conn, "unused.xlsx", "legacy-lspdfr").unwrap_err();

        assert!(
            err.contains("only available for the Legacy SP page"),
            "{err}"
        );
    }

    #[test]
    fn inspect_path_lookup_releases_the_database_lock_before_classification() {
        let connection = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        complete_onboarding_impl(&connection, saved_game_dir.path().to_str()).unwrap();
        let connection = std::sync::Mutex::new(connection);

        let path = effective_game_path_for_inspect(&connection, None, "legacy-sp").unwrap();

        assert_eq!(path.as_deref(), saved_game_dir.path().to_str());
        assert!(connection.try_lock().is_ok());
    }

    #[test]
    fn inspect_mod_impl_rejects_a_page_that_does_not_match_the_game_edition() {
        let game_dir = fake_enhanced_game_root();
        let err = inspect_mod_impl(
            Some(game_dir.path().to_str().unwrap()),
            "legacy-sp",
            "missing.asi",
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
    }

    #[test]
    fn inspect_mod_for_settings_rejects_an_invalid_saved_path_without_fallback() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let invalid_parent = tempfile::tempdir().unwrap();
        let invalid_path = invalid_parent.path().join("missing-game");
        complete_onboarding_impl(&conn, invalid_path.to_str()).unwrap();

        let err =
            inspect_mod_for_settings_impl(&conn, None, "legacy-sp", "missing.asi").unwrap_err();

        assert!(err.contains("supported GTA V install"), "{err}");
    }

    #[test]
    fn inspect_mod_for_settings_rejects_when_no_gta_path_is_configured() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();

        let err =
            inspect_mod_for_settings_impl(&conn, None, "legacy-sp", "missing.asi").unwrap_err();

        assert!(
            err.contains("Choose a GTA V installation path in Settings"),
            "{err}"
        );
    }

    #[test]
    fn fivem_does_not_inherit_the_saved_gta_game_path_override() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let path = effective_game_path_impl(&conn, None, gtavmm_core::providers::Mode::FivemClient)
            .unwrap();

        assert!(path.is_none());
    }

    #[test]
    fn pending_fivem_preview_preserves_both_roots() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let client_dir = tempfile::tempdir().unwrap();
        let mod_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("ClientMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let err = inspect_mod_for_settings_impl(
            &conn,
            client_dir.path().to_str(),
            "fivem-client",
            asi_path.to_str().unwrap(),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert!(!client_dir.path().join("ClientMod.asi").exists());
        assert!(!saved_game_dir.path().join("ClientMod.asi").exists());
    }

    #[test]
    fn fivem_preview_rejects_a_missing_explicit_client_path_even_with_a_saved_gta_path() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("ClientMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let err =
            inspect_mod_for_settings_impl(&conn, None, "fivem-client", asi_path.to_str().unwrap())
                .unwrap_err();

        assert!(err.contains("under development"), "{err}");
    }

    #[test]
    fn a_profile_round_trips_through_a_file_and_reports_what_is_missing() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode)
             VALUES ('Menyoo', 'folder', '/x', 'active', 'legacy-sp')",
            [],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let profile_id = gtavmm_core::profile::create(&conn, "Roleplay").unwrap();
        gtavmm_core::profile::add_mod(&conn, profile_id, mod_id).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.json");
        profile_export_impl(&conn, profile_id, file.to_str().unwrap()).unwrap();

        // Import onto a machine that has none of those mods.
        let other = gtavmm_core::db::open_in_memory().unwrap();
        let outcome = profile_import_impl(&other, file.to_str().unwrap()).unwrap();
        assert!(outcome.matched.is_empty());
        assert_eq!(
            outcome.not_found_locally,
            vec!["Menyoo".to_string()],
            "a mod that is not installed here must be reported, never fetched"
        );
    }

    #[test]
    fn importing_something_that_is_not_a_profile_export_says_so() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        std::fs::write(&file, b"{\"nope\": 1}").unwrap();
        let err = profile_import_impl(&conn, file.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a profile export file"), "{err}");
    }

    #[test]
    fn each_page_sees_only_its_own_mods() {
        // The defect this whole schema version exists to fix: without a mode
        // filter every page ran the same query and showed the same list.
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        for (name, mode) in [
            ("Menyoo", "legacy-sp"),
            ("Better Chases", "legacy-lspdfr"),
            ("Some resource", "fivem-client"),
        ] {
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode)
                 VALUES (?1, 'folder', '/x', 'active', ?2)",
                rusqlite::params![name, mode],
            )
            .unwrap();
        }

        let names = |mode: Option<&str>| -> Vec<String> {
            list_mods_impl(&conn, mode)
                .unwrap()
                .into_iter()
                .map(|m| m.name)
                .collect()
        };
        assert_eq!(names(Some("legacy-sp")), vec!["Menyoo"]);
        assert_eq!(names(Some("legacy-lspdfr")), vec!["Better Chases"]);
        assert!(list_mods_impl(&conn, Some("enhanced-sp")).is_err());
        // No filter still means everything — the profile membership table and
        // the global search both depend on that.
        assert_eq!(names(None).len(), 3);
    }

    #[test]
    fn correcting_a_mode_moves_the_mod_and_clears_the_guess_marker() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode, mode_inferred)
             VALUES ('Menyoo', 'folder', '/x', 'active', 'legacy-sp', 1)",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();

        set_mod_mode_impl(&conn, id, "legacy-lspdfr").unwrap();

        let moved = list_mods_impl(&conn, Some("legacy-lspdfr")).unwrap();
        assert_eq!(moved.len(), 1);
        assert!(!moved[0].mode_inferred, "a correction is not a guess");
        assert!(list_mods_impl(&conn, Some("legacy-sp")).unwrap().is_empty());
    }

    #[test]
    fn pending_mod_cannot_be_relabelled_to_bypass_its_restriction() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('Pending', 'asi', '/unused', 'active', 'fivem-client')",
            [],
        ).unwrap();
        let id = conn.last_insert_rowid();
        assert!(set_mod_mode_impl(&conn, id, "legacy-sp").is_err());
        let mode: String = conn
            .query_row(
                "SELECT mode FROM installed_mod WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mode, "fivem-client");
    }

    #[test]
    fn setting_an_unknown_mode_is_refused_rather_than_stored() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode)
             VALUES ('Menyoo', 'folder', '/x', 'active', 'legacy-sp')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        assert!(set_mod_mode_impl(&conn, id, "not-a-page").is_err());
        assert!(set_mod_mode_impl(&conn, 9999, "legacy-sp").is_err());
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    #[test]
    fn install_mod_impl_refuses_empty_plan_and_preserves_state() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let game = fake_game_root();
        let package = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let backup = work.path().join("backups");
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        let error = install_mod_impl(
            &mut conn,
            Some(game.path().to_str().unwrap()),
            "legacy-sp",
            package.path().to_str().unwrap(),
            None,
            false,
            &backup,
        )
        .unwrap_err();

        assert_eq!(error, "unsupported mod format: the install plan contains no files");
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
        assert_eq!(std::fs::read(game.path().join("GTA5.exe")).unwrap(), b"");
        assert_eq!(std::fs::read_dir(game.path()).unwrap().count(), 1);
        assert!(std::fs::read_dir(package.path()).unwrap().next().is_none());
        assert!(!backup.exists());
        assert!(conn.is_autocommit());
    }

    #[test]
    fn install_mod_impl_writes_the_file_and_records_it() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();

        let outcome = install_mod_impl(
            &mut conn,
            Some(game_dir.path().to_str().unwrap()),
            "legacy-sp",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap();

        match outcome {
            gtavmm_core::install::InstallOutcome::Success { files_written, .. } => {
                assert_eq!(files_written, 1);
            }
            other => panic!("expected Success, got {other:?}"),
        }
        assert!(game_dir.path().join("SomeMod.asi").exists());
        assert_eq!(list_mods_impl(&conn, None).unwrap().len(), 1);
    }

    #[test]
    fn install_mod_impl_uses_the_saved_game_path_override() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        install_mod_impl(
            &mut conn,
            None,
            "legacy-sp",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap();

        assert!(saved_game_dir.path().join("SomeMod.asi").exists());
    }

    #[test]
    fn pending_fivem_install_preserves_files_and_database() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let client_dir = tempfile::tempdir().unwrap();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("ClientMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let err = install_mod_impl(
            &mut conn,
            client_dir.path().to_str(),
            "fivem-client",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert!(!client_dir.path().join("ClientMod.asi").exists());
        assert!(!saved_game_dir.path().join("ClientMod.asi").exists());
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn fivem_install_rejects_a_missing_explicit_client_path_without_using_gta_path() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("ClientMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let err = install_mod_impl(
            &mut conn,
            None,
            "fivem-client",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert!(!saved_game_dir.path().join("ClientMod.asi").exists());
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn fivem_install_rejects_a_nonexistent_client_path_without_using_gta_path() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let saved_game_dir = fake_game_root();
        let parent_dir = tempfile::tempdir().unwrap();
        let missing_client_dir = parent_dir.path().join("missing-client");
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("ClientMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();
        complete_onboarding_impl(&conn, saved_game_dir.path().to_str()).unwrap();

        let err = install_mod_impl(
            &mut conn,
            missing_client_dir.to_str(),
            "fivem-client",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert!(!saved_game_dir.path().join("ClientMod.asi").exists());
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn install_mod_impl_rejects_when_no_gta_path_is_configured() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();

        let err = install_mod_impl(
            &mut conn,
            None,
            "legacy-sp",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap_err();

        assert!(
            err.contains("Choose a GTA V installation path in Settings"),
            "{err}"
        );
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn install_mod_impl_rejects_a_game_edition_mismatch_before_writing() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let game_dir = fake_enhanced_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let asi_path = mod_dir.path().join("SomeMod.asi");
        std::fs::write(&asi_path, b"fake asi bytes").unwrap();

        let err = install_mod_impl(
            &mut conn,
            Some(game_dir.path().to_str().unwrap()),
            "legacy-sp",
            asi_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert!(!game_dir.path().join("SomeMod.asi").exists());
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn install_mod_impl_rejects_an_unsupported_extension_before_touching_the_filesystem() {
        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        let game_dir = fake_game_root();
        let mod_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let weird_path = mod_dir.path().join("mystery.bin");
        std::fs::write(&weird_path, b"???").unwrap();

        let err = install_mod_impl(
            &mut conn,
            Some(game_dir.path().to_str().unwrap()),
            "legacy-sp",
            weird_path.to_str().unwrap(),
            None,
            false,
            backup_dir.path(),
        )
        .unwrap_err();
        assert!(err.contains("unsupported"));
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    fn insert_mod_row(conn: &Connection, name: &str, status: &str) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES (?1, 'asi', '', ?2)",
            rusqlite::params![name, status],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn profile_lifecycle_create_membership_switch() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let staging = tempfile::tempdir().unwrap();

        let profile_id = profile_create_impl(&conn, "Roleplay").unwrap();
        assert_eq!(profile_list_impl(&conn).unwrap().len(), 1);

        let mod_id = insert_mod_row(&conn, "SomeMod", "disabled");
        // OpenAI-Codex_fixture_review: Prepare a verifiable synthetic payload.
        let game = tempfile::tempdir().unwrap();
        let game_root = game.path();
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod SET install_path = ?1 WHERE id = ?2",
                rusqlite::params![
                    game_root.to_str().expect("synthetic game root must be Unicode"),
                    mod_id,
                ],
            )
            .unwrap(),
            1
        );
        let staged_dir = staging.path().join(mod_id.to_string());
        std::fs::create_dir_all(&staged_dir).unwrap();
        let staged = staged_dir.join("0");
        std::fs::write(&staged, b"synthetic mod payload").unwrap();
        let hash = gtavmm_core::util::hash_file(&staged).unwrap();
        let target = game_root.join("SomeMod.asi");
        assert!(matches!(
            std::fs::symlink_metadata(staged_dir.join("manifest.json")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(matches!(
            std::fs::symlink_metadata(&target),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert_eq!(
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    mod_id,
                    target.to_str().expect("synthetic target must be Unicode"),
                    hash,
                ],
            )
            .unwrap(),
            1
        );
        gtavmm_core::providers::ensure_installed_mod_available(&conn, mod_id).unwrap();
        profile_add_mod_impl(&conn, profile_id, mod_id).unwrap();
        assert_eq!(
            profile_mod_ids_impl(&conn, profile_id).unwrap(),
            vec![mod_id]
        );

        // OpenAI-Codex_fixture_review: Switch a verified synthetic payload.
        let outcome = profile_switch_impl(&conn, profile_id, staging.path()).unwrap();
        assert_eq!(outcome.enabled, vec![mod_id]);
        assert!(outcome.disabled.is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), b"synthetic mod payload");
        assert!(matches!(
            std::fs::symlink_metadata(&staged),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        let status: String = conn
            .query_row("SELECT status FROM installed_mod WHERE id = ?1", [mod_id], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "active");
        assert!(profile_list_impl(&conn).unwrap()[0].is_active);

        profile_remove_mod_impl(&conn, profile_id, mod_id).unwrap();
        assert!(profile_mod_ids_impl(&conn, profile_id).unwrap().is_empty());

        profile_delete_impl(&conn, profile_id).unwrap();
        assert!(profile_list_impl(&conn).unwrap().is_empty());
        let active_profile: Option<i64> = conn
            .query_row("SELECT active_profile_id FROM user_settings WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(active_profile, None);
        assert_eq!(std::fs::read(&target).unwrap(), b"synthetic mod payload");
    }

    // OpenAI-Codex_fixture_review: Zero-file rejection must reach the staging guard.
    #[test]
    fn profile_switch_rejects_disabled_mod_without_file_rows() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let staging_root = temp.path().join("staging");
        std::fs::create_dir_all(&game_root).unwrap();

        let mod_id = insert_mod_row(&conn, "NoFiles", "disabled");
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod SET install_path = ?1 WHERE id = ?2",
                rusqlite::params![
                    game_root.to_str().expect("synthetic game root must be Unicode"),
                    mod_id,
                ],
            )
            .unwrap(),
            1
        );
        let stage = staging_root.join(mod_id.to_string());
        std::fs::create_dir_all(&stage).unwrap();
        assert!(!stage.join("manifest.json").exists());
        assert!(std::fs::read_dir(&stage).unwrap().next().is_none());

        let profile_id = profile_create_impl(&conn, "Empty files").unwrap();
        profile_add_mod_impl(&conn, profile_id, mod_id).unwrap();
        assert_eq!(profile_mod_ids_impl(&conn, profile_id).unwrap(), vec![mod_id]);
        gtavmm_core::providers::ensure_installed_mod_available(&conn, mod_id).unwrap();

        let file_count = || {
            conn.query_row(
                "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = ?1",
                [mod_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(file_count(), 0);
        let active_before: Option<i64> = conn
            .query_row("SELECT active_profile_id FROM user_settings WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(active_before, None);
        let events_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
            .unwrap();

        let error = gtavmm_core::profile::switch(&conn, profile_id, &staging_root).unwrap_err();
        match error {
            gtavmm_core::error::CoreError::UnsupportedFormat { reason } => assert_eq!(
                reason,
                format!(
                    "disabled staging for mod {mod_id} is missing, unsafe, or does not match its file records; preserve it for recovery"
                )
            ),
            other => panic!("unexpected profile switch error: {other:?}"),
        }

        assert_eq!(file_count(), 0);
        let status: String = conn
            .query_row("SELECT status FROM installed_mod WHERE id = ?1", [mod_id], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "disabled");
        let active_after: Option<i64> = conn
            .query_row("SELECT active_profile_id FROM user_settings WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(active_after, active_before);
        let events_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events_after, events_before);
        assert_eq!(profile_mod_ids_impl(&conn, profile_id).unwrap(), vec![mod_id]);
        assert!(std::fs::read_dir(&stage).unwrap().next().is_none());
    }

    #[test]
    fn convert_vehicle_pack_impl_errors_cleanly_on_a_missing_dlc_rpf() {
        let dir = tempfile::tempdir().unwrap();
        let err = convert_vehicle_pack_impl(
            dir.path().join("nope.rpf").to_str().unwrap(),
            dir.path().join("out").to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("under development"));
    }

    #[test]
    fn language_defaults_to_en_and_round_trips_through_set() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        assert_eq!(get_language_impl(&conn).unwrap(), "en");

        set_language_impl(&conn, "zh-TW").unwrap();
        assert_eq!(get_language_impl(&conn).unwrap(), "zh-TW");
    }

    #[test]
    fn pending_fivem_load_order_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "core-lib", "fx_version 'cerulean'\n");
        write_manifest(dir.path(), "framework", "dependency 'core-lib'\n");

        let err = fivem_resolve_load_order_impl(dir.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("under development"), "{err}");
    }

    #[test]
    fn pending_fivem_load_order_preserves_server_cfg() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "core-lib", "fx_version 'cerulean'\n");
        let server_cfg = dir.path().join("server.cfg");
        std::fs::write(&server_cfg, "sv_hostname \"Test\"\n").unwrap();

        let err =
            fivem_apply_load_order_impl(dir.path().to_str().unwrap(), server_cfg.to_str().unwrap())
                .unwrap_err();
        assert!(err.contains("under development"), "{err}");

        let contents = std::fs::read_to_string(&server_cfg).unwrap();
        assert!(contents.contains("sv_hostname \"Test\""));
        assert_eq!(contents, "sv_hostname \"Test\"\n");
    }

    #[test]
    fn list_mods_impl_reads_real_rows_in_id_order() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('Mod A', 'asi', '', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('Mod B', 'dll', '', 'disabled')",
            [],
        )
        .unwrap();

        let mods = list_mods_impl(&conn, None).unwrap();
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].name, "Mod A");
        assert_eq!(mods[0].status, ModStatus::Active);
        assert_eq!(mods[1].name, "Mod B");
        assert_eq!(mods[1].status, ModStatus::Disabled);
    }

    #[test]
    fn list_mods_impl_empty_db_returns_empty_vec() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        assert!(list_mods_impl(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn detect_game_result_serializes_with_a_tagged_shape() {
        let found = DetectGameResult::Found {
            install_path: "C:/Games/GTAV".to_string(),
            edition: "legacy".to_string(),
        };
        let json = serde_json::to_string(&found).unwrap();
        assert_eq!(
            json,
            r#"{"status":"found","install_path":"C:/Games/GTAV","edition":"legacy"}"#
        );
    }

    #[test]
    fn inspect_dll_impl_surfaces_a_readable_error_for_a_non_pe_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-dll.dll");
        std::fs::write(&path, b"not a real PE file").unwrap();
        let err = inspect_dll_impl(path.to_str().unwrap()).unwrap_err();
        assert!(err.contains("PE file"));
    }

    #[test]
    fn translate_dll_draft_impl_never_writes_when_the_source_file_is_invalid() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-dll.dll");
        std::fs::write(&path, b"not a real PE file").unwrap();
        let source_sha256 = gtavmm_core::hash_calculator::compute(&path).unwrap().sha256;
        assert!(
            translate_dll_draft_impl(&conn, path.to_str().unwrap(), "zh-TW", &source_sha256)
                .is_err()
        );
        assert!(!dir.path().join("not-a-dll.zh-TW.dll").exists());
    }

    #[test]
    fn patch_dll_translations_impl_never_writes_when_the_source_file_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-dll.dll");
        std::fs::write(&path, b"not a real PE file").unwrap();
        let source_sha256 = gtavmm_core::hash_calculator::compute(&path).unwrap().sha256;
        let result = patch_dll_translations_impl(
            path.to_str().unwrap(),
            "zh-TW",
            &source_sha256,
            vec!["手動翻譯".to_string()],
            None,
        );
        assert!(result.is_err());
        assert!(!dir.path().join("not-a-dll.zh-TW.dll").exists());
    }

    #[test]
    fn patch_dll_translations_impl_never_writes_to_a_custom_output_when_source_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-dll.dll");
        std::fs::write(&path, b"not a real PE file").unwrap();
        let custom_output = dir.path().join("elsewhere.dll");
        let source_sha256 = gtavmm_core::hash_calculator::compute(&path).unwrap().sha256;
        let result = patch_dll_translations_impl(
            path.to_str().unwrap(),
            "zh-TW",
            &source_sha256,
            vec!["手動翻譯".to_string()],
            Some(custom_output.to_str().unwrap()),
        );
        assert!(result.is_err());
        assert!(!custom_output.exists());
    }

    // -----------------------------------------------------------------------
    // Mod lifecycle. The core's own suite already covers the mechanics in
    // depth; what these check is that the command layer resolves paths and
    // arguments correctly, because that is the part the core tests cannot see
    // and the part that was missing entirely until now.
    // -----------------------------------------------------------------------

    /// Installs one .asi into a fake game root and returns everything the
    /// lifecycle commands need to act on it.
    fn installed_fixture() -> (
        Connection,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        i64,
    ) {
        let game_dir = fake_game_root();
        let work = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();

        let src = work.path().join("LifecycleMod.asi");
        std::fs::write(&src, b"fake asi bytes").unwrap();

        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        install_mod_impl(
            &mut conn,
            Some(game_dir.path().to_str().unwrap()),
            "legacy-sp",
            src.to_str().unwrap(),
            Some("LifecycleMod"),
            false,
            &work.path().join("backups"),
        )
        .unwrap();

        let id: i64 = conn
            .query_row("SELECT id FROM installed_mod LIMIT 1", [], |r| r.get(0))
            .unwrap();
        (conn, game_dir, work, staging, id)
    }

    fn status_of(conn: &Connection, id: i64) -> String {
        conn.query_row(
            "SELECT status FROM installed_mod WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code. Synthetic test setup only.
    fn seed_synthetic_single_file_disabled(
        conn: &Connection,
        mod_id: i64,
        game_dir: &tempfile::TempDir,
        staging: &tempfile::TempDir,
    ) {
        assert_eq!(status_of(conn, mod_id), "active");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "fixture requires exactly one tracked file");
        let (file_id, target_path, backup_path, recorded_hash): (i64, String, Option<String>, String) =
            conn.query_row(
                "SELECT id, target_path, backup_path, file_hash FROM installed_mod_file \
                 WHERE installed_mod_id = ?1 ORDER BY id",
                [mod_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert!(backup_path.is_none(), "fixture requires no overwritten original");
        let target = std::path::PathBuf::from(target_path);
        assert!(std::fs::symlink_metadata(&target).unwrap().file_type().is_file());
        assert!(
            std::fs::canonicalize(&target)
                .unwrap()
                .starts_with(std::fs::canonicalize(game_dir.path()).unwrap())
        );
        let mod_staging = staging.path().join(mod_id.to_string());
        std::fs::create_dir(&mod_staging).unwrap();
        let staged = mod_staging.join("0");
        assert!(!staged.exists());
        std::fs::rename(&target, &staged).unwrap();
        let actual_hash = gtavmm_core::util::hash_file(&staged).unwrap();
        assert_eq!(actual_hash, recorded_hash);
        let manifest = serde_json::json!([{
            "file_id": file_id,
            "index": 0,
            "hash": actual_hash,
        }]);
        std::fs::write(
            mod_staging.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1 AND status = 'active'",
                [mod_id],
            )
            .unwrap(),
            1
        );
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    #[test]
    fn disable_pending_returns_error_and_preserves_the_mod() {
        let (conn, game_dir, _work, staging, id) = installed_fixture();
        let target = game_dir.path().join("LifecycleMod.asi");
        let before = std::fs::read(&target).unwrap();

        let error = disable_mod_impl(&conn, id, staging.path()).unwrap_err();

        assert_eq!(
            error,
            "Mod disabling is unavailable. This feature is under development."
        );
        assert_eq!(status_of(&conn, id), "active");
        assert_eq!(std::fs::read(&target).unwrap(), before);
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    #[test]
    fn profile_switch_disable_pending_returns_code_and_preserves_the_mod() {
        let (conn, game_dir, _work, staging, id) = installed_fixture();
        let target = game_dir.path().join("LifecycleMod.asi");
        let before = std::fs::read(&target).unwrap();
        let old = profile_create_impl(&conn, "Old").unwrap();
        let new = profile_create_impl(&conn, "New").unwrap();
        profile_add_mod_impl(&conn, old, id).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id = ?1 WHERE id = 1",
                [old],
            )
            .unwrap(),
            1
        );

        let error = profile_switch_impl(&conn, new, staging.path()).unwrap_err();

        assert_eq!(error, "GTAVMM_PROFILE_MOD_DISABLING_UNAVAILABLE");
        assert_eq!(status_of(&conn, id), "active");
        assert_eq!(std::fs::read(&target).unwrap(), before);
        let active: Option<i64> = conn
            .query_row("SELECT active_profile_id FROM user_settings WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(active, Some(old));
    }

    #[test]
    fn enable_restores_a_synthetically_staged_file() {
        let (conn, game_dir, _work, staging, id) = installed_fixture();
        let deployed = game_dir.path().join("LifecycleMod.asi");
        assert!(deployed.exists(), "install should have written the file");

        seed_synthetic_single_file_disabled(&conn, id, &game_dir, &staging);
        assert_eq!(status_of(&conn, id), "disabled");
        assert!(
            !deployed.exists(),
            "a disabled mod must not leave its file in the game folder"
        );

        enable_mod_impl(&conn, id, staging.path()).unwrap();
        assert_eq!(status_of(&conn, id), "active");
        assert!(deployed.exists(), "enabling should put the file back");
    }

    #[test]
    fn enable_rejects_active_and_disable_rejects_active_and_disabled() {
        let (conn, game_dir, _work, staging, id) = installed_fixture();

        // Already active, so there is nothing to enable.
        let err = enable_mod_impl(&conn, id, staging.path()).unwrap_err();
        assert!(err.contains("not disabled"), "unexpected message: {err}");

        let target = game_dir.path().join("LifecycleMod.asi");
        let active_bytes = std::fs::read(&target).unwrap();
        let err = disable_mod_impl(&conn, id, staging.path()).unwrap_err();
        assert!(err.contains("Mod disabling is unavailable."), "unexpected message: {err}");
        assert_eq!(status_of(&conn, id), "active");
        assert_eq!(std::fs::read(&target).unwrap(), active_bytes);
        assert!(!staging.path().join(id.to_string()).exists());

        seed_synthetic_single_file_disabled(&conn, id, &game_dir, &staging);
        let staged = staging.path().join(id.to_string()).join("0");
        let staged_bytes = std::fs::read(&staged).unwrap();
        let err = disable_mod_impl(&conn, id, staging.path()).unwrap_err();
        assert!(err.contains("Mod disabling is unavailable."), "unexpected message: {err}");
        assert_eq!(status_of(&conn, id), "disabled");
        assert_eq!(std::fs::read(&staged).unwrap(), staged_bytes);
        assert!(!target.exists());
    }

    #[test]
    fn uninstall_removes_the_file_and_snapshots_it_into_the_recycle_bin() {
        let (mut conn, game_dir, work, _staging, id) = installed_fixture();
        let deployed = game_dir.path().join("LifecycleMod.asi");
        let recycle = work.path().join("recycle_bin");

        uninstall_mod_impl(
            &mut conn,
            id,
            Some(game_dir.path().to_str().unwrap()),
            &recycle,
        )
        .unwrap();

        assert_eq!(status_of(&conn, id), "uninstalled");
        assert!(
            !deployed.exists(),
            "uninstall should delete the deployed file"
        );

        let entries = gtavmm_core::recycle_bin::list(&conn).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "uninstall must leave something restorable behind"
        );
    }

    #[test]
    fn uninstalling_an_unknown_id_errors_rather_than_silently_doing_nothing() {
        let (mut conn, game_dir, work, _staging, _id) = installed_fixture();
        let err = uninstall_mod_impl(
            &mut conn,
            9999,
            Some(game_dir.path().to_str().unwrap()),
            &work.path().join("recycle_bin"),
        )
        .unwrap_err();
        assert!(err.contains("9999"), "unexpected message: {err}");
    }

    // OpenAI-Codex: Cover page-aware uninstall routing for the FiveM client root.
    #[test]
    fn pending_fivem_uninstall_preserves_deployed_files() {
        let client_dir = tempfile::tempdir().unwrap();
        let deployed = client_dir.path().join("client-mod.asi");
        std::fs::write(&deployed, b"FiveM client mod").unwrap();

        let mut conn = gtavmm_core::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Client Mod', 'asi', ?1, 'active', 'fivem-client')",
            [deployed.to_string_lossy().as_ref()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_id, deployed.to_string_lossy().as_ref()],
        )
        .unwrap();

        let err = uninstall_mod_for_page_impl(
            &mut conn,
            mod_id,
            Some(client_dir.path().to_str().unwrap()),
            "fivem-client",
            &client_dir.path().join("recycle_bin"),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert_eq!(std::fs::read(&deployed).unwrap(), b"FiveM client mod");
        assert_eq!(status_of(&conn, mod_id), "active");
        assert!(gtavmm_core::recycle_bin::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn disabled_uninstall_uses_explicit_staging_for_both_routes() {
        // OpenAI-Codex: Exercise the same helper used by the Tauri command.
        for mode in [None, Some("legacy-sp")] {
            let (mut conn, game_dir, work, staging, id) = installed_fixture();
            seed_synthetic_single_file_disabled(&conn, id, &game_dir, &staging);
            let recycle = work.path().join("disabled-recycle");
            uninstall_mod_with_staging_impl(
                &mut conn,
                id,
                Some(game_dir.path().to_str().unwrap()),
                mode,
                &recycle,
                staging.path(),
            )
            .unwrap();
            assert_eq!(status_of(&conn, id), "uninstalled");
            assert!(!game_dir.path().join("LifecycleMod.asi").exists());
            assert_eq!(gtavmm_core::recycle_bin::list(&conn).unwrap().len(), 1);
        }
    }

    #[test]
    fn reinstall_replaces_the_file_from_a_new_source() {
        let (mut conn, game_dir, work, _staging, id) = installed_fixture();
        let deployed = game_dir.path().join("LifecycleMod.asi");

        let newer = work.path().join("v2").join("LifecycleMod.asi");
        std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
        std::fs::write(&newer, b"newer asi bytes").unwrap();

        reinstall_mod_impl(
            &mut conn,
            id,
            newer.to_str().unwrap(),
            "2.0",
            "legacy-sp",
            Some(game_dir.path().to_str().unwrap()),
            &work.path().join("backups-reinstall"),
            &work.path().join("recycle_bin"),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(&deployed).unwrap(),
            b"newer asi bytes",
            "the deployed file should be the new source's contents"
        );
    }

    #[test]
    fn reinstall_uses_the_saved_path_when_no_path_is_passed() {
        let (mut conn, game_dir, work, _staging, id) = installed_fixture();
        complete_onboarding_impl(&conn, game_dir.path().to_str()).unwrap();
        let deployed = game_dir.path().join("LifecycleMod.asi");
        let newer = work.path().join("v2").join("LifecycleMod.asi");
        std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
        std::fs::write(&newer, b"saved-root reinstall").unwrap();

        reinstall_mod_impl(
            &mut conn,
            id,
            newer.to_str().unwrap(),
            "2.1",
            "legacy-sp",
            None,
            &work.path().join("backups-saved-root"),
            &work.path().join("recycle-bin-saved-root"),
        )
        .unwrap();

        assert_eq!(std::fs::read(&deployed).unwrap(), b"saved-root reinstall");
    }

    #[test]
    fn reinstall_rejects_the_wrong_saved_edition_before_uninstalling() {
        let (mut conn, game_dir, work, _staging, id) = installed_fixture();
        let enhanced_game_dir = fake_enhanced_game_root();
        complete_onboarding_impl(&conn, enhanced_game_dir.path().to_str()).unwrap();
        let deployed = game_dir.path().join("LifecycleMod.asi");
        let before = std::fs::read(&deployed).unwrap();

        let err = reinstall_mod_impl(
            &mut conn,
            id,
            "missing-new-source.asi",
            "2.1",
            "legacy-sp",
            None,
            &work.path().join("backups-wrong-edition"),
            &work.path().join("recycle-bin-wrong-edition"),
        )
        .unwrap_err();

        assert!(err.contains("under development"), "{err}");
        assert_eq!(std::fs::read(&deployed).unwrap(), before);
        assert_eq!(status_of(&conn, id), "active");
    }

    #[test]
    fn reinstall_rejects_an_unknown_mode_before_touching_anything() {
        let (mut conn, game_dir, work, _staging, id) = installed_fixture();
        let deployed = game_dir.path().join("LifecycleMod.asi");
        let before = std::fs::read(&deployed).unwrap();

        let err = reinstall_mod_impl(
            &mut conn,
            id,
            "irrelevant.asi",
            "2.0",
            "not-a-mode",
            Some(game_dir.path().to_str().unwrap()),
            &work.path().join("backups-reinstall"),
            &work.path().join("recycle_bin"),
        )
        .unwrap_err();

        assert!(
            err.contains("unknown page mode"),
            "unexpected message: {err}"
        );
        assert_eq!(
            std::fs::read(&deployed).unwrap(),
            before,
            "a rejected mode must not have disturbed the installed file"
        );
        assert_eq!(status_of(&conn, id), "active");
    }

    // -----------------------------------------------------------------------
    // Settings and startup state.
    // -----------------------------------------------------------------------

    #[test]
    fn a_fresh_install_starts_at_the_terms_gate_following_the_system_theme() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let state = load_startup_state_impl(&conn).unwrap();
        assert_eq!(state.theme, "system", "an unset theme means follow the OS");
        assert!(!state.terms_accepted);
        assert!(!state.onboarding_completed);
    }

    #[test]
    fn accepting_terms_then_finishing_setup_moves_startup_past_both_gates() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        accept_terms_impl(&conn).unwrap();
        let state = load_startup_state_impl(&conn).unwrap();
        assert!(state.terms_accepted);
        assert!(
            !state.onboarding_completed,
            "accepting the terms must not also count as finishing setup"
        );

        complete_onboarding_impl(&conn, Some(r"D:\Games\GTAV")).unwrap();
        let state = load_startup_state_impl(&conn).unwrap();
        assert!(state.onboarding_completed);
        assert_eq!(
            load_user_settings_impl(&conn)
                .unwrap()
                .game_install_path_override
                .as_deref(),
            Some(r"D:\Games\GTAV")
        );
    }

    #[test]
    fn finishing_setup_without_a_path_keeps_the_one_already_stored() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        complete_onboarding_impl(&conn, Some(r"D:\Games\GTAV")).unwrap();
        // Passing nothing means "leave it alone", not "clear it".
        complete_onboarding_impl(&conn, None).unwrap();
        assert_eq!(
            load_user_settings_impl(&conn)
                .unwrap()
                .game_install_path_override
                .as_deref(),
            Some(r"D:\Games\GTAV")
        );
    }

    #[test]
    fn set_theme_round_trips_and_rejects_anything_else() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        set_theme_impl(&conn, "dark").unwrap();
        assert_eq!(load_startup_state_impl(&conn).unwrap().theme, "dark");
        set_theme_impl(&conn, "system").unwrap();
        assert_eq!(load_startup_state_impl(&conn).unwrap().theme, "system");

        let err = set_theme_impl(&conn, "solarized").unwrap_err();
        assert!(err.contains("unknown theme"), "unexpected message: {err}");
        assert_eq!(
            load_startup_state_impl(&conn).unwrap().theme,
            "system",
            "a rejected theme must not have been written"
        );
    }

    #[test]
    fn updating_backup_settings_leaves_the_gates_untouched() {
        // A panel showing two fields must not be able to reset the terms
        // acceptance or the first-run state by writing back a partial row.
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        accept_terms_impl(&conn).unwrap();
        complete_onboarding_impl(&conn, None).unwrap();

        update_user_settings_impl(
            &conn,
            Some(false),
            PathChange::Keep,
            PathChange::Set {
                path: r"E:\Backups".to_string(),
            },
        )
        .unwrap();

        let s = load_user_settings_impl(&conn).unwrap();
        assert!(!s.default_auto_backup);
        assert_eq!(s.backup_root_override.as_deref(), Some(r"E:\Backups"));

        let state = load_startup_state_impl(&conn).unwrap();
        assert!(state.terms_accepted, "terms acceptance was clobbered");
        assert!(state.onboarding_completed, "onboarding state was clobbered");
    }

    #[test]
    fn validate_game_path_reports_not_found_for_a_folder_without_a_game() {
        let dir = tempfile::tempdir().unwrap();
        let result = validate_game_path_impl(dir.path().to_str().unwrap()).unwrap();
        assert!(matches!(result, DetectGameResult::NotFound));
    }

    #[test]
    fn validate_game_path_recognises_a_real_looking_install() {
        let dir = fake_game_root();
        let result = validate_game_path_impl(dir.path().to_str().unwrap()).unwrap();
        assert!(matches!(result, DetectGameResult::Found { .. }));
    }

    #[test]
    fn clearing_a_path_is_distinguishable_from_leaving_it_alone() {
        // The reason PathChange is an explicit three-way rather than a nested
        // Option: serde reads a JSON null for Option<Option<String>> as the
        // outer None, so "clear this" would arrive indistinguishable from
        // "leave it alone" and the clear button would silently do nothing.
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        update_user_settings_impl(
            &conn,
            None,
            PathChange::Set {
                path: r"D:\Games\GTAV".to_string(),
            },
            PathChange::Keep,
        )
        .unwrap();

        // Keep leaves it standing.
        update_user_settings_impl(&conn, None, PathChange::Keep, PathChange::Keep).unwrap();
        assert_eq!(
            load_user_settings_impl(&conn)
                .unwrap()
                .game_install_path_override
                .as_deref(),
            Some(r"D:\Games\GTAV")
        );

        // Clear actually removes it.
        update_user_settings_impl(&conn, None, PathChange::Clear, PathChange::Keep).unwrap();
        assert!(load_user_settings_impl(&conn)
            .unwrap()
            .game_install_path_override
            .is_none());
    }
}
