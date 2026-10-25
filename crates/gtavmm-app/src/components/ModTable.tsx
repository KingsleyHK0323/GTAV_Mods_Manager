import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { ModDetailDrawer } from "./ModDetailDrawer";
import type { InstalledMod, PageMode } from "../types";

interface ModTableProps {
  mods: InstalledMod[];
  onChanged: () => void;
  /** Forwarded to lifecycle calls; GTA pages use the saved path when this is null. */
  gamePath?: string | null;
  /** Which provider a reinstall from this page should use. */
  mode?: PageMode;
  /** LSPDFR pages add a Category column. */
  showCategory?: boolean;
  /** OpenAI-Codex: Show batch state controls for selected rows. */
  bulkStateActions?: boolean;
}

/**
 * The mod table shared by every workspace page that lists `installed_mod` rows
 * (Legacy/Enhanced SP, Legacy/Enhanced LSPDFR — FiveM Client deliberately does
 * not use this, see its own page for why).
 *
 * A row opens the detail drawer rather than expanding an editor inline. The
 * inline row could only edit notes and the source link; the drawer is also
 * where disable, enable, reinstall and uninstall live, so there is one place to
 * act on a mod instead of an editor here and actions elsewhere.
 */
export function ModTable({
  mods,
  onChanged,
  gamePath = null,
  mode = "legacy-sp",
  showCategory = false,
  bulkStateActions = false,
}: ModTableProps) {
  const { t } = useTranslation();
  const [openId, setOpenId] = useState<number | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  const [bulkBusy, setBulkBusy] = useState<"enable" | "disable" | null>(null);
  const [bulkError, setBulkError] = useState<string | null>(null);
  const [bulkStatus, setBulkStatus] = useState<string | null>(null);

  const statusLabel: Record<InstalledMod["status"], string> = {
    Active: t("legacySp.status_active"),
    Disabled: t("legacySp.status_disabled"),
    Uninstalled: t("legacySp.status_uninstalled"),
  };
  // "pill--off" rather than "pill disabled": the design renamed it so a mod the
  // user switched off is not described with the same word as a control that
  // cannot be used. The value is a complete class list, not a modifier appended
  // to "pill", which is why "active" keeps its leading "pill ".
  const statusClass: Record<InstalledMod["status"], string> = {
    Active: "pill active",
    Disabled: "pill pill--off",
    Uninstalled: "pill pill--off",
  };

  // Read from the freshest list rather than holding a copy, so the drawer shows
  // the new status straight after an action instead of the pre-action snapshot.
  const openMod = mods.find((m) => m.id === openId) ?? null;
  const selectableMods = mods.filter((mod) => mod.status !== "Uninstalled");
  const selectedMods = selectableMods.filter((mod) => selectedIds.has(mod.id));
  const enableCandidates = selectedMods.filter((mod) => mod.status === "Disabled");
  const disableCandidates = selectedMods.filter((mod) => mod.status === "Active");
  const allSelectableSelected =
    selectableMods.length > 0 && selectableMods.every((mod) => selectedIds.has(mod.id));

  async function applyBulkState(action: "enable" | "disable") {
    const candidates = action === "enable" ? enableCandidates : disableCandidates;
    if (candidates.length === 0 || bulkBusy) return;

    setBulkBusy(action);
    setBulkError(null);
    setBulkStatus(null);
    const failures: Array<{ id: number; detail: string }> = [];
    let succeeded = 0;
    const command = action === "enable" ? "enable_mod" : "disable_mod";

    for (const mod of candidates) {
      try {
        await invoke(command, { modId: mod.id });
        succeeded += 1;
      } catch (error) {
        failures.push({ id: mod.id, detail: `${mod.name}: ${String(error)}` });
      }
    }

    setSelectedIds(new Set(failures.map((failure) => failure.id)));
    onChanged();
    if (failures.length > 0) {
      const key = action === "enable" ? "bulk_enable_error" : "bulk_disable_error";
      setBulkError(t(`legacySp.${key}`, {
        count: failures.length,
        details: failures.map((failure) => failure.detail).join("; "),
      }));
    } else {
      const key = action === "enable" ? "bulk_enabled" : "bulk_disabled";
      setBulkStatus(t(`legacySp.${key}`, { count: succeeded }));
    }
    setBulkBusy(null);
  }

  return (
    <>
      <div style={{ overflowX: "auto" }}>
        {bulkStateActions && (
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: "var(--space-2)",
              flexWrap: "wrap",
              marginBottom: "var(--space-3)",
            }}
          >
            <label style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
              <input
                type="checkbox"
                checked={allSelectableSelected}
                disabled={selectableMods.length === 0 || bulkBusy !== null}
                aria-label={t("legacySp.select_all_mods")}
                onChange={(event) => {
                  setSelectedIds((current) => {
                    const next = new Set(current);
                    for (const mod of selectableMods) {
                      if (event.target.checked) next.add(mod.id);
                      else next.delete(mod.id);
                    }
                    return next;
                  });
                }}
              />
              {t("legacySp.select_all_mods")}
            </label>
            <span>{t("legacySp.selected_mods", { count: selectedMods.length })}</span>
            <div style={{ display: "flex", gap: "var(--space-2)", flexWrap: "wrap" }}>
              <button
                className="btn-ghost"
                type="button"
                disabled={enableCandidates.length === 0 || bulkBusy !== null}
                onClick={() => applyBulkState("enable")}
              >
                {bulkBusy === "enable"
                  ? t("legacySp.bulk_enabling")
                  : t("legacySp.bulk_enable_selected", { count: enableCandidates.length })}
              </button>
              <button
                className="btn-ghost"
                type="button"
                disabled={disableCandidates.length === 0 || bulkBusy !== null}
                onClick={() => applyBulkState("disable")}
              >
                {bulkBusy === "disable"
                  ? t("legacySp.bulk_disabling")
                  : t("legacySp.bulk_disable_selected", { count: disableCandidates.length })}
              </button>
            </div>
          </div>
        )}
        {bulkError && <p className="error" role="alert">{bulkError}</p>}
        {bulkStatus && <p style={{ color: "var(--success)" }}>{bulkStatus}</p>}
        <table className="data">
        <thead>
          <tr>
            {bulkStateActions && <th>{t("legacySp.select_column")}</th>}
            <th>{t("legacySp.col_mod")}</th>
            {showCategory && <th>{t("modWorkspace.col_category")}</th>}
            <th>{t("legacySp.col_status")}</th>
            <th>{t("legacySp.col_installed")}</th>
            <th>{t("legacySp.col_root")}</th>
            <th>{t("legacySp.col_link")}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {mods.map((m) => (
            <tr key={m.id}>
              {bulkStateActions && (
                <td>
                  <input
                    type="checkbox"
                    checked={selectedIds.has(m.id)}
                    disabled={m.status === "Uninstalled" || bulkBusy !== null}
                    aria-label={t("legacySp.select_mod", { name: m.name })}
                    onChange={(event) => {
                      setSelectedIds((current) => {
                        const next = new Set(current);
                        if (event.target.checked) next.add(m.id);
                        else next.delete(m.id);
                        return next;
                      });
                    }}
                  />
                </td>
              )}
              <td>
                <div className="mod-name">{m.name}</div>
                <div className="mod-type">.{m.source_type}</div>
              </td>
              {showCategory && (
                <td>{m.category ? t(`modWorkspace.cat_${m.category}`) : "—"}</td>
              )}
              <td>
                <span className={statusClass[m.status]}>{statusLabel[m.status]}</span>
              </td>
              <td className="mono">{m.installed_at.slice(0, 10)}</td>
              <td className="path mono">{m.install_path}</td>
              <td>
                {m.link ? (
                  <a
                    href={m.link}
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
              <td style={{ textAlign: "right" }}>
                <button className="icon-btn" type="button" onClick={() => setOpenId(m.id)}>
                  {t("drawer.details")}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
        </table>
      </div>

      <ModDetailDrawer
        mod={openMod}
        gamePath={gamePath}
        mode={mode}
        onClose={() => setOpenId(null)}
        onChanged={onChanged}
      />
    </>
  );
}
