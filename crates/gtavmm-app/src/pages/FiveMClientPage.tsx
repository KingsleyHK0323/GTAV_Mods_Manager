import { useCallback, useEffect, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Icon } from "../components/IconSprite";
import { InstallWizard } from "../components/InstallWizard";
import { ModDetailDrawer } from "../components/ModDetailDrawer";
import { RecycleBinModal } from "../components/RecycleBinModal";
import { pickFolder } from "../lib/pickers";
import type { InstalledMod, RecycleBinEntry } from "../types";

/** OpenAI-Codex: Keep installed rows scoped to the FiveM Client page mode. */
export function FiveMClientPage() {
  const { t } = useTranslation();
  const [clientPath, setClientPath] = useState("");
  const [savedClientPath, setSavedClientPath] = useState("");
  const [clientPathLoaded, setClientPathLoaded] = useState(false);
  const [pathSaving, setPathSaving] = useState(false);
  const [pathSaved, setPathSaved] = useState(false);
  const [pathLoadError, setPathLoadError] = useState<string | null>(null);
  const [pathSaveError, setPathSaveError] = useState<string | null>(null);
  const [wizardOpen, setWizardOpen] = useState(false);
  const [selectedModId, setSelectedModId] = useState<number | null>(null);
  const [mods, setMods] = useState<InstalledMod[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [recycleEntries, setRecycleEntries] = useState<RecycleBinEntry[] | null>(null);
  const [recycleError, setRecycleError] = useState<string | null>(null);
  const [recycleModalOpen, setRecycleModalOpen] = useState(false);

  const loadMods = useCallback((preserveCurrentList = false) => {
    if (!preserveCurrentList) setMods(null);
    setLoadError(null);
    invoke<InstalledMod[]>("list_mods", { mode: "fivem-client" })
      .then(setMods)
      .catch((error) => setLoadError(String(error)));
  }, []);

  const loadRecycleBin = useCallback(() => {
    setRecycleEntries(null);
    setRecycleError(null);
    invoke<RecycleBinEntry[]>("list_recycle_bin", { mode: "fivem-client" })
      .then(setRecycleEntries)
      .catch((error) => setRecycleError(String(error)));
  }, []);

  const refreshModsAndRecycleBin = useCallback(() => {
    loadMods(true);
    loadRecycleBin();
  }, [loadMods, loadRecycleBin]);

  async function saveClientPath(path = clientPath, showSavedNotice = true): Promise<boolean> {
    const candidate = path.trim();
    if (!candidate) return false;

    setPathSaving(true);
    setPathSaveError(null);
    setPathSaved(false);
    try {
      const saved = await invoke<string>("save_fivem_client_path", { path: candidate });
      setClientPath(saved);
      setSavedClientPath(saved);
      setPathLoadError(null);
      setPathSaved(showSavedNotice);
      return true;
    } catch (error) {
      setPathSaveError(String(error));
      return false;
    } finally {
      setPathSaving(false);
    }
  }

  useEffect(() => {
    loadMods();
    loadRecycleBin();
    invoke<string | null>("load_fivem_client_path")
      .then((path) => {
        const saved = path ?? "";
        setSavedClientPath(saved);
        setClientPath((current) => (current.trim() ? current : saved));
      })
      .catch((error) => setPathLoadError(String(error)))
      .finally(() => setClientPathLoaded(true));
  }, [loadMods, loadRecycleBin]);

  const selectedMod = mods?.find((mod) => mod.id === selectedModId) ?? null;

  const statusLabel: Record<InstalledMod["status"], string> = {
    Active: t("legacySp.status_active"),
    Disabled: t("legacySp.status_disabled"),
    Uninstalled: t("legacySp.status_uninstalled"),
  };
  const statusClass: Record<InstalledMod["status"], string> = {
    Active: "pill active",
    Disabled: "pill pill--off",
    Uninstalled: "pill pill--off",
  };

  return (
    <section className="view" data-shown="true">
      <div className="page-head">
        <div>
          <h1 className="page-title">{t("fivemClient.title")}</h1>
          <p className="page-sub">
            <Trans
              i18nKey="fivemClient.subtitle"
              components={{ mono: <span className="mono" />, em: <em /> }}
            />
          </p>
        </div>
      </div>

      <div className="path-picker">
        <span className="path-icon">
          <Icon name="folder" />
        </span>
        <div className="path-text">
          <span className="label">{t("fivemClient.folder_label")}</span>
          <input
            className="mono"
            style={{
              background: "transparent",
              border: "none",
              color: "var(--text)",
              width: "100%",
              outline: "none",
            }}
            value={clientPath}
            disabled={!clientPathLoaded || pathSaving}
            onChange={(e) => {
              setClientPath(e.target.value);
              setPathSaved(false);
              setPathSaveError(null);
            }}
            placeholder={t("fivemClient.folder_placeholder")}
          />
        </div>
        <button
          className="btn-ghost"
          type="button"
          disabled={!clientPathLoaded || pathSaving}
          onClick={async () => {
            const picked = await pickFolder(t("fivemClient.picker_title"));
            if (picked) {
              setClientPath(picked);
              await saveClientPath(picked);
            }
          }}
        >
          {t("fivemClient.browse")}
        </button>
        <button
          className="btn-ghost"
          type="button"
          onClick={() => saveClientPath()}
          disabled={
            !clientPathLoaded ||
            pathSaving ||
            !clientPath.trim() ||
            clientPath.trim() === savedClientPath
          }
        >
          {pathSaving ? t("fivemClient.saving_path") : t("fivemClient.save_path")}
        </button>
        <button
          className="btn-primary"
          type="button"
          disabled={!clientPathLoaded || pathSaving || !clientPath.trim()}
          onClick={async () => {
            if (!(await saveClientPath(clientPath, false))) return;
            setWizardOpen(true);
          }}
        >
          {t("legacySp.install_mod_button")}
        </button>
      </div>
      {pathLoadError && (
        <p className="error" role="alert">
          {t("fivemClient.path_load_error", { error: pathLoadError })}
        </p>
      )}
      {pathSaveError && (
        <p className="error" role="alert">
          {t("fivemClient.path_save_error", { error: pathSaveError })}
        </p>
      )}
      {pathSaved && (
        <p style={{ color: "var(--success)", fontSize: "var(--fs-xs)" }}>
          {t("fivemClient.path_saved")}
        </p>
      )}

      <InstallWizard
        open={wizardOpen}
        onClose={() => setWizardOpen(false)}
        onInstalled={refreshModsAndRecycleBin}
        mode="fivem-client"
        gamePath={clientPath.trim()}
      />

      <div className="info-banner">
        <svg className="icon glyph" aria-hidden="true">
          <use href="#i-info" />
        </svg>
        <span>
          <Trans
            i18nKey="fivemClient.unverified_assumption"
            components={{ mono: <span className="mono" />, strong: <strong /> }}
          />
        </span>
      </div>

      <div className="panel">
        <div className="panel-head">
          <h2>{t("fivemClient.panel_title")}</h2>
        </div>
        {loadError && (
          <p className="error" role="alert">
            {t("fivemClient.load_error", { error: loadError })}{" "}
            <button className="btn-ghost" type="button" onClick={() => loadMods()}>
              {t("fivemClient.retry")}
            </button>
          </p>
        )}
        {mods === null && !loadError && (
          <p style={{ padding: "16px 20px" }}>{t("fivemClient.loading")}</p>
        )}
        {mods !== null && mods.length === 0 && (
          <div className="empty-state">
            <span className="glyph">
              <Icon name="folder" />
            </span>
            <h3>{t("fivemClient.empty_title")}</h3>
            <p>{t("modWorkspace.empty_body")}</p>
          </div>
        )}
        {mods !== null && mods.length > 0 && (
          <div style={{ overflowX: "auto" }}>
            <table className="data">
              <thead>
                <tr>
                  <th>{t("legacySp.col_mod")}</th>
                  <th>{t("legacySp.col_status")}</th>
                  <th>{t("legacySp.col_installed")}</th>
                  <th>{t("legacySp.col_root")}</th>
                  <th>{t("legacySp.col_link")}</th>
                  <th>{t("drawer.details")}</th>
                </tr>
              </thead>
              <tbody>
                {mods.map((mod) => (
                  <tr key={mod.id}>
                    <td>
                      <div className="mod-name">{mod.name}</div>
                      <div className="mod-type">.{mod.source_type}</div>
                    </td>
                    <td>
                      <span className={statusClass[mod.status]}>{statusLabel[mod.status]}</span>
                    </td>
                    <td className="mono">{mod.installed_at.slice(0, 10)}</td>
                    <td className="path mono">{mod.install_path}</td>
                    <td>
                      {mod.link ? (
                        <a
                          href={mod.link}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="mono"
                          style={{ fontSize: "var(--fs-xs)" }}
                        >
                          {t("legacySp.open_link")}
                        </a>
                      ) : (
                        <span className="path">—</span>
                      )}
                    </td>
                    <td>
                      <button
                        className="icon-btn"
                        type="button"
                        onClick={() => setSelectedModId(mod.id)}
                      >
                        {t("drawer.details")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <div className="panel">
        <div className="panel-head">
          <h2>{t("modPageTools.recycle_title")}</h2>
          <button
            className="btn-ghost"
            type="button"
            onClick={() => setRecycleModalOpen(true)}
            disabled={!recycleEntries}
          >
            {t("modPageTools.recycle_view_all_button")}
          </button>
        </div>
        {recycleError && (
          <p className="error" role="alert">
            {t("fivemClient.recycle_load_error", { error: recycleError })}{" "}
            <button className="btn-ghost" type="button" onClick={loadRecycleBin}>
              {t("fivemClient.retry")}
            </button>
          </p>
        )}
        {recycleEntries === null && !recycleError && (
          <p style={{ padding: "16px 20px" }}>{t("fivemClient.recycle_loading")}</p>
        )}
        {recycleEntries && recycleEntries.length === 0 && (
          <p style={{ padding: "16px 20px", color: "var(--text-faint)" }}>
            {t("modPageTools.recycle_empty")}
          </p>
        )}
        {recycleEntries?.slice(0, 2).map((entry) => (
          <div className="recycle-item" key={entry.id}>
            <span className="name">#{entry.id}</span>
            <span className="expiry">
              {t("modPageTools.recycle_expires", { date: entry.expires_at.slice(0, 10) })}
            </span>
          </div>
        ))}
      </div>

      <RecycleBinModal
        open={recycleModalOpen}
        onClose={() => setRecycleModalOpen(false)}
        entries={recycleEntries ?? []}
        onRestored={refreshModsAndRecycleBin}
      />

      <ModDetailDrawer
        mod={selectedMod}
        gamePath={clientPath.trim() || null}
        mode="fivem-client"
        onClose={() => setSelectedModId(null)}
        onChanged={refreshModsAndRecycleBin}
      />
    </section>
  );
}
