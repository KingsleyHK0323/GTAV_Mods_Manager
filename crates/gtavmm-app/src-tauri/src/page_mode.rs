// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Which workspace page a mod belongs to.
//!
//! This is deliberately a different thing from [`gtavmm_core::providers::Mode`],
//! which answers "how do I install this" and has three values (`Sp`, `Lspdfr`,
//! `FivemClient`). A page is finer: Legacy SP and Enhanced SP both install
//! through the SP provider but are separate workspaces with separate mod lists,
//! so the provider mode alone cannot say which page a mod came from.
//!
//! Keeping the two apart lets the shared provider layouts remain separate from the
//! page identity, while the page identity verifies the selected GTA edition.
//!
//! The page also records its expected GTA V edition so a saved or auto-detected
//! path for the other edition can be rejected before the install preview or write.

use gtavmm_core::providers::Mode;

/// A workspace page that owns a list of installed mods.
///
/// FiveM Server is absent on purpose: that page resolves a load order over
/// files someone else put in `resources\`, it never installs a mod of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageMode {
    LegacySp,
    LegacyLspdfr,
    EnhancedSp,
    EnhancedLspdfr,
    FivemClient,
}

impl PageMode {
    /// The stored form, matching what schema v11 writes into `installed_mod.mode`.
    pub fn as_str(self) -> &'static str {
        match self {
            PageMode::LegacySp => "legacy-sp",
            PageMode::LegacyLspdfr => "legacy-lspdfr",
            PageMode::EnhancedSp => "enhanced-sp",
            PageMode::EnhancedLspdfr => "enhanced-lspdfr",
            PageMode::FivemClient => "fivem-client",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "legacy-sp" => Ok(PageMode::LegacySp),
            "legacy-lspdfr" => Ok(PageMode::LegacyLspdfr),
            "enhanced-sp" => Ok(PageMode::EnhancedSp),
            "enhanced-lspdfr" => Ok(PageMode::EnhancedLspdfr),
            "fivem-client" => Ok(PageMode::FivemClient),
            other => Err(format!("unknown page mode: {other}")),
        }
    }

    /// Which provider installs for this page.
    pub fn provider_mode(self) -> Mode {
        match self {
            PageMode::LegacySp | PageMode::EnhancedSp => Mode::Sp,
            PageMode::LegacyLspdfr | PageMode::EnhancedLspdfr => Mode::Lspdfr,
            PageMode::FivemClient => Mode::FivemClient,
        }
    }

    /// OpenAI-Codex: Edition required by this GTA V workspace; FiveM has no GTA edition.
    pub fn expected_game_edition(self) -> Option<&'static str> {
        match self {
            PageMode::LegacySp | PageMode::LegacyLspdfr => Some("legacy"),
            PageMode::EnhancedSp | PageMode::EnhancedLspdfr => Some("enhanced"),
            PageMode::FivemClient => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_round_trips_through_its_stored_form() {
        for page in [
            PageMode::LegacySp,
            PageMode::LegacyLspdfr,
            PageMode::EnhancedSp,
            PageMode::EnhancedLspdfr,
            PageMode::FivemClient,
        ] {
            assert_eq!(PageMode::parse(page.as_str()), Ok(page));
        }
    }

    #[test]
    fn the_two_sp_pages_share_a_provider_but_are_different_pages() {
        // This is the whole reason a page is not the same thing as a provider
        // mode: the provider cannot tell these two mod lists apart, so a column
        // holding only the provider mode could not either.
        assert_eq!(
            PageMode::LegacySp.provider_mode(),
            PageMode::EnhancedSp.provider_mode()
        );
        assert_ne!(PageMode::LegacySp.as_str(), PageMode::EnhancedSp.as_str());
    }

    #[test]
    fn each_gta_page_requires_its_own_edition_and_fivem_requires_none() {
        assert_eq!(PageMode::LegacySp.expected_game_edition(), Some("legacy"));
        assert_eq!(PageMode::LegacyLspdfr.expected_game_edition(), Some("legacy"));
        assert_eq!(PageMode::EnhancedSp.expected_game_edition(), Some("enhanced"));
        assert_eq!(PageMode::EnhancedLspdfr.expected_game_edition(), Some("enhanced"));
        assert_eq!(PageMode::FivemClient.expected_game_edition(), None);
    }

    #[test]
    fn an_unknown_page_is_an_error_rather_than_a_default() {
        assert!(PageMode::parse("legacy").is_err());
        assert!(PageMode::parse("sp").is_err());
        assert!(PageMode::parse("fivem-server").is_err());
    }
}
