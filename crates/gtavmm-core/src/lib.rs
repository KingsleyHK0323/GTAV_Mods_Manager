// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_dlc_deep_audit

//! Core business logic for GTAV Mods Manager: game detection, mod install/uninstall,
//! conflict detection, recycle bin, and settings. No UI or CLI dependencies.
//!
//! MVP scope: Legacy SP Mods only. See `providers` for the `ModeProvider` abstraction
//! that later modes (Enhanced SP, LSPDFR, FiveM) will implement additively.
//!
//! ```rust
//! let _ = std::mem::size_of::<gtavmm_core::lml_store::Phase>();
//! ```
//!
//! ```compile_fail
//! use gtavmm_core::lml_install::stage_package;
//! ```

pub mod ai_assistant;
pub mod app_log;
pub mod components;
pub mod conflict;
pub mod crash_report;
pub mod db;
pub mod dlc_manual;
pub mod dlclist_editor;
pub mod dll_translation;
pub mod error;
pub mod fivem;
pub mod full_backup;
pub mod game_locator;
pub mod hash_calculator;
pub mod history;
pub mod install;
// OpenAI-Codex: Declaration-only LML foundation; no installation entry point.
pub mod lml;
// OpenAI-Codex: Source inventory and limited declaration comparisons only.
pub mod lml_package;
// OpenAI-Codex: Database-only operation records; no filesystem installation API.
pub mod lml_store;
// OpenAI-Codex: Shared checked path helpers for LML package inspection.
mod lml_fs;
// OpenAI-Codex: Experimental staging is limited to unit-test builds.
#[cfg(test)]
mod lml_install;
pub mod malware_scan;
pub mod mod_analyzer;
pub mod mod_details;
pub mod mod_search;
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code
pub mod mod_source_check;
pub mod profile;
pub mod prompt_template;
pub mod protected_files;
pub mod providers;
pub mod recycle_bin;
pub mod saved_links;
pub mod settings;
pub mod sp_to_fivem;
pub mod state;
pub mod translation;
pub mod uninstall;
pub mod update_check;
pub mod util;
pub mod xlsx_export;

pub use error::CoreError;
