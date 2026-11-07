import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { IconSprite, Icon } from "./components/IconSprite";
import { Sidebar } from "./components/Sidebar";
import { LegacySpPage } from "./pages/LegacySpPage";
import { LegacyLspdfrPage } from "./pages/LegacyLspdfrPage";
import { EnhancedSpPage } from "./pages/EnhancedSpPage";
import { EnhancedLspdfrPage } from "./pages/EnhancedLspdfrPage";
import { FiveMClientPage } from "./pages/FiveMClientPage";
import { FiveMServerPage } from "./pages/FiveMServerPage";
import { FiveMConverterPage } from "./pages/FiveMConverterPage";
import { SettingsPage } from "./pages/SettingsPage";
import { ProfilesPage } from "./pages/ProfilesPage";
import { DllTranslationPage } from "./pages/DllTranslationPage";
import { ActivityLogPage } from "./pages/ActivityLogPage";
import { SavedLinksPage } from "./pages/SavedLinksPage";
import { ToolsPage } from "./pages/ToolsPage";
import { PlaceholderPage } from "./pages/PlaceholderPage";
import type { Mode, ModSearchResult, Sub } from "./types";
import "./styles/mockup.css";
import "./App.css";
import { TermsGate } from "./pages/TermsGate";
import { OnboardingGate } from "./pages/OnboardingGate";
import { applyTheme, type Theme } from "./lib/theme";
import startupEnglish from "./i18n/locales/en.json";
import startupTraditionalChinese from "./i18n/locales/zh-TW.json";

// OpenAI-Codex: Keep FiveM Server gated until the user re-enables support.
const FIVEM_SERVER_SUPPORTED = false;

/* The accent is switched by setting data-mode on the app root and letting the
   stylesheet do the rest — mockup.css carries a [data-mode="…"] rule per mode
   that assigns --accent and --accent-soft. The previous approach wrote an
   inline style referencing --accent-legacy and friends, which silently broke
   when the design renamed those to --mode-legacy: var() resolved to nothing,
   so --accent was empty and every component reading it lost its colour. An
   attribute cannot fail that way — if the mode is wrong the rule simply does
   not match, which is visible immediately rather than degrading to blank. */

/** What `load_startup_state` returns — which gate (if any) to show first. */
interface StartupState {
  theme: string;
  terms_accepted: boolean;
  onboarding_completed: boolean;
  language: string;
}

function pageFor(mode: Mode, sub: Sub) {
  if (mode === "legacy" && sub === "mods") return <LegacySpPage />;
  if (mode === "legacy" && sub === "lspdfr") return <LegacyLspdfrPage />;
  if (mode === "enhanced" && sub === "mods") return <EnhancedSpPage />;
  if (mode === "enhanced" && sub === "lspdfr") return <EnhancedLspdfrPage />;
  if (mode === "fivem" && sub === "client") return <FiveMClientPage />;
  if (mode === "fivem" && sub === "server") {
    return FIVEM_SERVER_SUPPORTED ? (
      <FiveMServerPage />
    ) : (
      <PlaceholderPage title="FiveM · Server" comingSoon />
    );
  }
  if (mode === "fivem" && sub === "converter") return <FiveMConverterPage />;
  const titles: Record<string, string> = {
    "legacy-lspdfr": "LSPDFR · Legacy",
    "enhanced-mods": "SP Mods · Enhanced",
    "enhanced-lspdfr": "LSPDFR · Enhanced",
    "fivem-client": "FiveM · Client",
    "fivem-server": "FiveM · Server",
    "fivem-converter": "FiveM · Converter",
  };
  return <PlaceholderPage title={titles[`${mode}-${sub}`] ?? `${mode} / ${sub}`} />;
}

function App() {
  const { t, i18n } = useTranslation();
  const [mode, setMode] = useState<Mode>("legacy");
  const [sub, setSub] = useState<Sub>("mods");
  type Overlay =
    | "settings"
    | "profiles"
    | "dllTranslation"
    | "activityLog"
    | "savedLinks"
    | "tools"
    | null;
  const [overlay, setOverlay] = useState<Overlay>(null);

  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<ModSearchResult[] | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);

  // Real, fully local keyword search (gtavmm_core::mod_search) — not natural-language
  // understanding, see that module's own doc comment for why. Debounced by a plain
  // timeout since there's no dedicated search-hook infrastructure in this app yet.
  useEffect(() => {
    if (!searchQuery.trim()) {
      setSearchResults(null);
      return;
    }
    const handle = setTimeout(() => {
      invoke<ModSearchResult[]>("search_mods", { query: searchQuery })
        .then(setSearchResults)
        .catch(() => setSearchResults(null));
    }, 200);
    return () => clearTimeout(handle);
  }, [searchQuery]);

  // One startup read decides what to show first — the terms gate, first-run
  // setup, or the workspace — and carries the stored language and theme with it,
  // so the shell does not make three separate round trips to find out.
  //
  // `null` means "not known yet". The workspace is not rendered until the answer
  // arrives, because flashing it and then replacing it with a gate would show
  // mod management to someone who has not accepted the terms.
  // OpenAI-Codex_v1_asi_rebase: Do not bypass gates when startup fails.
  const [startup, setStartup] = useState<StartupState | null>(null);
  const [startupError, setStartupError] = useState(false);
  const [startupAttempt, setStartupAttempt] = useState(0);

  useEffect(() => {
    let active = true;
    setStartup(null);
    setStartupError(false);

    async function loadStartup() {
      try {
        const state = await invoke<StartupState>("load_startup_state");
        if (
          !state ||
          typeof state.theme !== "string" ||
          typeof state.language !== "string" ||
          typeof state.terms_accepted !== "boolean" ||
          typeof state.onboarding_completed !== "boolean"
        ) {
          throw new Error("Invalid startup state");
        }
        if (!active) return;
        if (state.language) await i18n.changeLanguage(state.language);
        if (!active) return;
        applyTheme(state.theme as Theme);
        setStartup(state);
      } catch {
        if (active) {
          setStartup(null);
          setStartupError(true);
        }
      }
    }

    void loadStartup();
    return () => {
      active = false;
    };
  }, [i18n, startupAttempt]);

  if (!startup) {
    return (
      <div className="fullscreen" data-shown="true">
        <div className="fullscreen-inner" style={{ maxWidth: 660, minWidth: 0 }}>
          {startupError ? (
            <>
              <h1 className="page-title">
                {startupEnglish.startup.error_title} / {startupTraditionalChinese.startup.error_title}
              </h1>
              <p className="error" role="alert">
                {startupEnglish.startup.error_body}
                <br />
                {startupTraditionalChinese.startup.error_body}
              </p>
              <button
                className="btn-primary"
                type="button"
                onClick={() => {
                  setStartupError(false);
                  setStartupAttempt((value) => value + 1);
                }}
              >
                {startupEnglish.startup.retry} / {startupTraditionalChinese.startup.retry}
              </button>
            </>
          ) : (
            <p role="status">
              {startupEnglish.startup.loading} / {startupTraditionalChinese.startup.loading}
            </p>
          )}
        </div>
      </div>
    );
  }
  if (!startup.terms_accepted) {
    return (
      <TermsGate onAccepted={() => setStartup({ ...startup, terms_accepted: true })} />
    );
  }
  if (!startup.onboarding_completed) {
    return (
      <OnboardingGate onDone={() => setStartup({ ...startup, onboarding_completed: true })} />
    );
  }

  // LSPDFR has no accent of its own: it is a sub-tab under both editions, so a
  // page beneath it keeps its parent edition's colour and carries its identity
  // through the gradient badge and shield icon instead.
  return (
    <div className="app" data-mode={mode}>
      <IconSprite />
      <Sidebar
        mode={mode}
        sub={sub}
        onSelect={(m, s) => {
          setMode(m);
          setSub(s);
          setOverlay(null);
        }}
        onOpenSettings={() => setOverlay("settings")}
        onOpenProfiles={() => setOverlay("profiles")}
        onOpenDllTranslation={() => setOverlay("dllTranslation")}
        onOpenActivityLog={() => setOverlay("activityLog")}
        onOpenSavedLinks={() => setOverlay("savedLinks")}
        onOpenTools={() => setOverlay("tools")}
      />
      <main className="main">
        <div className="topbar">
          <div className="crumb">
            {overlay === "settings" ? (
              <strong>{t("nav.settings")}</strong>
            ) : overlay === "profiles" ? (
              <strong>{t("nav.profiles")}</strong>
            ) : overlay === "dllTranslation" ? (
              <strong>{t("nav.dll_translation")}</strong>
            ) : overlay === "activityLog" ? (
              <strong>{t("nav.activity_log")}</strong>
            ) : overlay === "savedLinks" ? (
              <strong>{t("nav.saved_links")}</strong>
            ) : overlay === "tools" ? (
              <strong>{t("nav.tools")}</strong>
            ) : (
              <>
                <strong>{t(`nav.${mode}`)}</strong>
                <span className="sep">/</span>
                {t(`nav.${sub === "mods" ? "sp_mods" : sub}`)}
              </>
            )}
          </div>
          <div className="dropdown" data-open={String(searchOpen)} style={{ marginLeft: "auto" }}>
            <div className="search">
              <Icon name="search" />
              <input
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onFocus={() => setSearchOpen(true)}
                onBlur={() => setTimeout(() => setSearchOpen(false), 150)}
                placeholder={t("topbar.search_placeholder")}
                style={{
                  background: "none",
                  border: "none",
                  outline: "none",
                  color: "inherit",
                  font: "inherit",
                  width: "100%",
                }}
              />
            </div>
            {searchOpen && searchQuery.trim() && (
              <div className="dd-menu" style={{ width: 260, maxHeight: 280, overflowY: "auto" }}>
                {searchResults === null && <div className="dd-option">{t("topbar.search_loading")}</div>}
                {searchResults && searchResults.length === 0 && (
                  <div className="dd-option">{t("topbar.search_empty")}</div>
                )}
                {searchResults?.map((r) => (
                  <div className="dd-option" key={r.id} style={{ flexDirection: "column", alignItems: "stretch" }}>
                    <span style={{ fontWeight: 600 }}>{r.name}</span>
                    <span style={{ fontSize: 11, color: "var(--text-faint)" }}>{r.status}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
        <div className="content">
          {overlay === "settings" ? (
            <SettingsPage />
          ) : overlay === "profiles" ? (
            <ProfilesPage />
          ) : overlay === "dllTranslation" ? (
            <DllTranslationPage />
          ) : overlay === "activityLog" ? (
            <ActivityLogPage />
          ) : overlay === "savedLinks" ? (
            <SavedLinksPage />
          ) : overlay === "tools" ? (
            <ToolsPage />
          ) : (
            pageFor(mode, sub)
          )}
        </div>
      </main>
    </div>
  );
}

export default App;

