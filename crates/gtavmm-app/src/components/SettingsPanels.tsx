import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { SettingsModal } from "./SettingsModal";
import { applyTheme, type Theme } from "../lib/theme";
import { pickFolder } from "../lib/pickers";

// OpenAI-Codex_ui_report_review: Permit only confirmed opt-out while backups are paused.
function isBackupSettingsPending(): boolean { return true; }

/** The whole `user_settings` row, as `load_user_settings` returns it. */
interface UserSettings {
  language: string;
  default_auto_backup: boolean;
  game_install_path_override: string | null;
  theme: string | null;
  terms_accepted_version: string | null;
  onboarding_completed: boolean;
  backup_root_override: string | null;
}

type DetectResult =
  | { status: "found"; install_path: string; edition: string }
  | { status: "not_found" };

/**
 * Theme.
 *
 * Applies the moment it is picked so the choice can be judged rather than
 * imagined, and persists the preference rather than the resolved palette — so
 * "System" keeps following the OS instead of freezing today's answer.
 */
export function ThemePanel({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [choice, setChoice] = useState<Theme>("system");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<{ theme: string }>("load_startup_state")
      .then((s) => setChoice((s.theme ?? "system") as Theme))
      .catch(() => {
        // No Tauri runtime — leave the control on its default rather than
        // pretending a stored value was read.
      });
  }, []);

  async function choose(next: Theme) {
    setChoice(next);
    applyTheme(next);
    setError(null);
    try {
      await invoke("set_theme", { theme: next });
    } catch (e) {
      setError(String(e));
    }
  }

  const options: { value: Theme; icon: string }[] = [
    { value: "system", icon: "#i-monitor" },
    { value: "dark", icon: "#i-moon" },
    { value: "light", icon: "#i-sun" },
  ];

  return (
    <SettingsModal title={t("settingsCards.theme_title")} wide onClose={onClose}>
      {error && <p className="error">{error}</p>}
      <div className="radio-row">
        {options.map((o) => (
          <button
            key={o.value}
            className="radio-card"
            type="button"
            aria-pressed={choice === o.value}
            onClick={() => choose(o.value)}
          >
            <svg className="icon icon-lg" aria-hidden="true">
              <use href={o.icon} />
            </svg>
            <span className="rc-title">{t(`settingsCards.theme_${o.value}`)}</span>
            <span className="rc-desc">{t(`settingsCards.theme_${o.value}_desc`)}</span>
          </button>
        ))}
      </div>
      <p className="page-sub">{t("settingsCards.theme_help")}</p>
    </SettingsModal>
  );
}

/**
 * Game paths.
 *
 * This exists so moving an install is something the application can be told
 * about. Without it the stored path would go stale silently and mods would keep
 * being written to the old location.
 */
export function GamePathsPanel({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<UserSettings | null>(null);
  const [detected, setDetected] = useState<DetectResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    invoke<UserSettings>("load_user_settings").then(setSettings).catch((e) => setError(String(e)));
    invoke<DetectResult>("detect_game").then(setDetected).catch(() => setDetected(null));
  }, []);

  async function browse() {
    const picked = await pickFolder(t("onboarding.browse_title"));
    if (!picked) return;
    setError(null);
    try {
      const result = await invoke<DetectResult>("validate_game_path", { path: picked });
      if (result.status === "not_found") {
        setError(t("onboarding.not_a_game_folder"));
        return;
      }
      setBusy(true);
      const updated = await invoke<UserSettings>("update_user_settings", {
        gameInstallPathOverride: { action: "set", path: picked },
      });
      setSettings(updated);
      setSaved(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function clearOverride() {
    setBusy(true);
    setError(null);
    try {
      // An explicit "clear" action rather than a null: a null would be read as
      // "leave it alone" and this button would appear to do nothing.
      const updated = await invoke<UserSettings>("update_user_settings", {
        gameInstallPathOverride: { action: "clear" },
      });
      setSettings(updated);
      setSaved(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const override = settings?.game_install_path_override ?? null;
  const auto = detected?.status === "found" ? detected : null;

  return (
    <SettingsModal title={t("settingsCards.paths_title")} wide onClose={onClose}>
      {error && <p className="error">{error}</p>}

      <div className="info-banner warn" style={{ marginBottom: "var(--gap)" }}>
        <svg className="icon glyph" aria-hidden="true">
          <use href="#i-alert-triangle" />
        </svg>
        <span>{t("settingsCards.paths_move_warning")}</span>
      </div>

      <div className="field-group" style={{ marginBottom: "var(--gap)" }}>
        <label>{t("settingsCards.paths_detected")}</label>
        <div className="path-picker">
          <svg className="icon path-icon" aria-hidden="true">
            <use href="#i-folder" />
          </svg>
          <div className="path-text">
            {auto ? (
              <span className="value mono">{auto.install_path}</span>
            ) : (
              <span className="value mono empty">{t("settingsCards.paths_none_detected")}</span>
            )}
          </div>
        </div>
      </div>

      <div className="field-group" style={{ marginBottom: "var(--gap)" }}>
        <label>{t("settingsCards.paths_override")}</label>
        <div className="path-picker">
          <svg className="icon path-icon" aria-hidden="true">
            <use href="#i-folder" />
          </svg>
          <div className="path-text">
            {override ? (
              <span className="value mono">{override}</span>
            ) : (
              <span className="value mono empty">{t("settingsCards.paths_no_override")}</span>
            )}
          </div>
          <button className="btn-ghost" type="button" onClick={browse} disabled={busy}>
            {t("onboarding.change")}
          </button>
          {override && (
            <button className="icon-btn" type="button" onClick={clearOverride} disabled={busy}>
              {t("settingsCards.paths_clear")}
            </button>
          )}
        </div>
      </div>

      <p className="page-sub">{t("settingsCards.paths_override_help")}</p>
      {saved && <p style={{ color: "var(--success)" }}>{t("settings.saved")}</p>}
    </SettingsModal>
  );
}

// OpenAI-Codex: Distinguish saved preferences from current App behavior.
// OpenAI-Codex_Responsible: Source_Code; OpenAI-Codex_dialog_evidence.
function isBackupConfirmationCancelled(error: unknown): boolean {
  return typeof error === "object" && error !== null &&
    "code" in error && error.code === "backup_confirmation_cancelled";
}

/** Backup defaults and where full backups are written. */
export function BackupPanel({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<UserSettings | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [settingsReady, setSettingsReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const lifecycle = useRef(0);
  const readGeneration = useRef(0);
  const updateInFlight = useRef(false);

  useEffect(() => {
    const epoch = ++lifecycle.current;
    const refresh = () => {
      const request = ++readGeneration.current;
      setLoadError(null);
      setLoading(true);
      setSettingsReady(false);
      void invoke<UserSettings>("load_user_settings").then((value) => {
        if (lifecycle.current === epoch && readGeneration.current === request) {
          setSettings(value);
          setSettingsReady(true);
        }
      }).catch((e) => {
        if (lifecycle.current === epoch && readGeneration.current === request) setLoadError(String(e));
      }).finally(() => {
        if (lifecycle.current === epoch && readGeneration.current === request) setLoading(false);
      });
    };
    window.addEventListener("gtavmm:settings-changed", refresh);
    refresh();
    return () => {
      window.removeEventListener("gtavmm:settings-changed", refresh);
      lifecycle.current += 1;
      readGeneration.current += 1;
    };
  }, []);

  async function update(patch: Record<string, unknown>) {
    if (isBackupSettingsPending() && (patch.defaultAutoBackup !== false || Object.keys(patch).length !== 1)) {
      setError(t("settingsCards.backup_paused"));
      return;
    }
    if (updateInFlight.current) return;
    updateInFlight.current = true;
    const epoch = lifecycle.current;
    setBusy(true);
    setError(null);
    setNotice(null);
    readGeneration.current += 1;
    setSettingsReady(false);
    setLoading(false);
    try {
      await invoke<UserSettings>("update_user_settings", patch);
    } catch (e) {
      if (lifecycle.current === epoch) {
        if (isBackupConfirmationCancelled(e)) setNotice(t("settingsCards.backup_change_cancelled"));
        else setError(String(e));
      }
    } finally {
      updateInFlight.current = false;
      window.dispatchEvent(new Event("gtavmm:settings-changed"));
      if (lifecycle.current === epoch) setBusy(false);
    }
  }

  async function chooseLocation() {
    if (isBackupSettingsPending()) return;
    const picked = await pickFolder(t("settingsCards.backup_pick_title"));
    if (!picked) return;
    await update({ backupRootOverride: { action: "set", path: picked } });
  }

  return (
    <SettingsModal title={t("settingsCards.backup_title")} wide onClose={onClose}>
      {isBackupSettingsPending() && <p role="status">{t("settingsCards.backup_paused")}</p>}
      {loadError !== null && <p className="error" role="alert">{loadError.trim().length > 0 ? loadError : t("settingsCards.backup_error_fallback")}</p>}
      {error !== null && <p className="error" role="alert">{error.trim().length > 0 ? error : t("settingsCards.backup_error_fallback")}</p>}
      {notice !== null && <p role="status">{notice}</p>}

      <div
        className="override-row"
        aria-busy={busy || loading}
        style={{ borderColor: "var(--border)", background: "var(--surface-2)", marginBottom: "var(--gap)" }}
      >
        {settings && (
          <fieldset disabled={isBackupSettingsPending() && !settings.default_auto_backup} style={{ border: 0, margin: 0, padding: 0 }}>
          <input
            type="checkbox"
            id="autoBackup"
            checked={settings.default_auto_backup}
            disabled={busy || loading || !settingsReady}
            onChange={(event) => void update({ defaultAutoBackup: event.currentTarget.checked })}
            aria-labelledby="backupAutoLabel"
            aria-describedby="backupAutoHelp"
          />
          </fieldset>
        )}
        <span id="backupAutoLabel">{t("settingsCards.backup_auto_label")}</span>
        <span role="status">
          {busy ? t("settingsCards.backup_change_pending")
            : loading ? t("settingsCards.backup_preference_loading")
            : !settingsReady || !settings
              ? t(settings ? "settingsCards.backup_last_loaded_unavailable" : "settingsCards.backup_preference_unavailable")
              : t(settings.default_auto_backup ? "settingsCards.backup_saved_on" : "settingsCards.backup_saved_off")}
        </span>
      </div>

      <p className="page-sub" id="backupAutoHelp">{t("settingsCards.backup_auto_help")}</p>

      <div className="field-group">
        <label>{t("settingsCards.backup_location")}</label>
        <div className="path-picker">
          <svg className="icon path-icon" aria-hidden="true">
            <use href="#i-folder" />
          </svg>
          <div className="path-text">
            {!settings ? (
              <span className="value mono empty">{t("settingsCards.backup_preference_unavailable")}</span>
            ) : settings.backup_root_override !== null ? (
              <span className="value mono">
                {settings.backup_root_override.length === 0
                  ? t("settingsCards.backup_empty_location")
                  : settings.backup_root_override.trim().length === 0
                    ? t("settingsCards.backup_whitespace_location")
                    : settings.backup_root_override}
              </span>
            ) : (
              <span className="value mono empty">{t("settingsCards.backup_default_location")}</span>
            )}
          </div>
          <button className="btn-ghost" type="button" onClick={chooseLocation} disabled>
            {t("onboarding.change")}
          </button>
          {settings && settings.backup_root_override !== null && (
            <button
              className="icon-btn"
              type="button"
              onClick={() => update({ backupRootOverride: { action: "clear" } })}
              disabled
            >
              {t("settingsCards.paths_clear")}
            </button>
          )}
        </div>
      </div>

      <div hidden={isBackupSettingsPending()}>
      <p className="page-sub">{t("settingsCards.backup_help")}</p>
      </div>
    </SettingsModal>
  );
}
