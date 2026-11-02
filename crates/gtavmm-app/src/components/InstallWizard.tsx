// OpenAI-Codex
// OpenAI-Codex_manual_dlc_ui
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { pickFile } from "../lib/pickers";
import { formatLabel, type InstallOutcome, type InstallPreview, type PageMode, type ScanOutcome } from "../types";

interface InstallWizardProps {
  open: boolean;
  onClose: () => void;
  /** Reloads files and reminders after any completed install attempt. */
  onInstalled: () => void;
  /**
   * Which page is installing. The backend derives the provider from it and
   * records it on the mod, so the mod appears on that page and nowhere else.
   */
  mode?: PageMode;
  /** Required for `fivem-client` (no auto-detection); ignored otherwise. */
  gamePath?: string | null;
}

type Step = "pick" | "analyzing" | "review" | "installing" | "done" | "error";

/**
 * The provider behind a page, for the copy that describes what to pick.
 *
 * Legacy SP and Enhanced SP want the same instructions — the difference between
 * those pages is which list the mod lands in, not what the user drags in.
 */
function providerOf(mode: PageMode): "sp" | "lspdfr" | "fivem-client" {
  if (mode === "fivem-client") return "fivem-client";
  return mode.endsWith("lspdfr") ? "lspdfr" : "sp";
}

/**
 * OpenAI-Codex: Mode-specific classification and real conflict preflight precede
 * installation. The preview is informational; install rechecks current targets.
 * Archive classification can use staging files, but never installs during preview.
 */
export function InstallWizard({
  open,
  onClose,
  onInstalled,
  mode = "legacy-sp",
  gamePath = null,
}: InstallWizardProps) {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>("pick");
  const [path, setPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<InstallPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<InstallOutcome | null>(null);
  const [scanResult, setScanResult] = useState<ScanOutcome | null>(null);
  const [scanning, setScanning] = useState(false);
  const generation = useRef(0);
  const inFlight = useRef<number | null>(null);
  const mounted = useRef(true);
  const context = useRef({ open, mode, gamePath });
  context.current = { open, mode, gamePath };
  const plan = preview?.plan ?? null;
  const blocked = !preview || preview.mode !== mode ||
    preview.conflicts.protected_hits.length > 0 || !plan || plan.files.length === 0 ||
    (typeof plan.format === "object" && "Unsupported" in plan.format);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current += 1; };
  }, []);
  useEffect(() => {
    reset();
  }, [open, mode, gamePath]);

  function reset() {
    generation.current += 1;
    inFlight.current = null;
    setStep("pick");
    setPath(null);
    setPreview(null);
    setError(null);
    setOutcome(null);
    setScanResult(null);
    setScanning(false);
  }

  function handleClose() {
    reset();
    onClose();
  }

  async function pickAndAnalyze() {
    if (inFlight.current !== null || !open) return;
    const request = ++generation.current;
    inFlight.current = request;
    const current = () => mounted.current && request === generation.current &&
      context.current.open && context.current.mode === mode && context.current.gamePath === gamePath;
    try {
      const picked = await pickFile(
        ["asi", "dll", "xml", "zip", "7z", "rar"],
        t("installWizard.pick_title"),
      );
      if (!picked || !current()) return;
      setPath(picked);
      setStep("analyzing");
      setError(null);
      setPreview(null);
      setScanResult(null);
      const result = await invoke<InstallPreview>("inspect_install_preview", { gamePath, mode, path: picked });
      if (!current()) return;
      if (result.mode !== mode) throw new Error(t("installWizard.preview_changed"));
      setPreview(result);
      setStep("review");
      // Fire-and-forget — a scan result (or its unavailability) is informational,
      // never blocks review/install. See gtavmm_core::malware_scan's own doc comment
      // for why this never claims more confidence than the OS scanner actually gives.
      setScanning(true);
      invoke<ScanOutcome>("scan_mod_path", { path: picked })
        .then((result) => { if (current()) setScanResult(result); })
        .catch(() => { if (current()) setScanResult({ Unavailable: { reason: "scan could not run" } }); })
        .finally(() => { if (current()) setScanning(false); });
    } catch (e) {
      if (current()) { setError(String(e)); setStep("error"); }
    } finally {
      if (inFlight.current === request) inFlight.current = null;
    }
  }

  async function runInstall(overrideForeignConflicts: boolean) {
    if (!path || !preview || blocked || inFlight.current !== null) return;
    if (overrideForeignConflicts && !(step === "done" && outcome && "RequiresOverride" in outcome)) return;
    const request = ++generation.current;
    inFlight.current = request;
    const current = () => mounted.current && request === generation.current &&
      context.current.open && context.current.mode === mode && context.current.gamePath === gamePath;
    setStep("installing");
    setError(null);
    try {
      const result = await invoke<InstallOutcome>("install_mod", {
        gamePath,
        mode,
        path,
        name: null,
        overrideForeignConflicts,
        expectedGameRoot: preview.game_root,
      });
      if (current()) { setOutcome(result); setStep("done"); }
    } catch (e) {
      if (current()) { setError(String(e)); setStep("error"); }
    } finally {
      if (inFlight.current === request) inFlight.current = null;
      onInstalled();
    }
  }

  if (!open) return null;

  return (
    <div className="modal-backdrop" data-open="true">
      <div className="modal">
        <div className="modal-head">
          <h2>{t("installWizard.title")}</h2>
          <button className="drawer-close" type="button" aria-label={t("installWizard.close")} disabled={step === "installing"} onClick={handleClose}>
            ×
          </button>
        </div>

        <div className="modal-body">
          {step === "pick" && (
            <>
              <p>{t(`installWizard.pick_body_${providerOf(mode)}`)}</p>
              <button className="btn-primary" type="button" onClick={pickAndAnalyze}>
                {t("installWizard.pick_button")}
              </button>
            </>
          )}

          {step === "analyzing" && <p>{t("installWizard.analyzing")}</p>}

          {step === "review" && plan && (
            <>
              <p>{t("installWizard.preview_mode", {
                mode: t(preview?.mode === "legacy-sp" ? "legacySp.title" : "legacyLspdfr.title"),
              })}</p>
              <p className="mono">{t("installWizard.preview_root", { path: preview?.game_root })}</p>
              <div className="analyze-row">
                <span>{t("installWizard.row_format")}</span>
                <span className="mono">{formatLabel(plan.format)}</span>
              </div>
              <div className="analyze-row">
                <span>{t("installWizard.row_files")}</span>
                <span className="mono">{plan.files.length}</span>
              </div>
              {plan.files.slice(0, 5).map((f, i) => (
                <div className="analyze-row" key={i}>
                  <span className="mono" style={{ fontSize: 11 }}>
                    {f.source.split(/[\\/]/).pop()}
                  </span>
                  <span className="mono" style={{ fontSize: 11 }}>
                    {f.target}
                  </span>
                </div>
              ))}
              {plan.files.length > 5 && (
                <p style={{ marginTop: 8 }}>
                  {t("installWizard.more_files", { count: plan.files.length - 5 })}
                </p>
              )}
              <p style={{ marginTop: 12 }}>{t("installWizard.review_note")}</p>
              <p>{t("installWizard.preflight_note")}</p>
              {preview && preview.conflicts.protected_hits.length > 0 && <div className="conflict-box" role="alert">
                <p>{t("installWizard.protected_body")}</p>
                <ul>{preview.conflicts.protected_hits.map((hit, index) => <li className="mono" key={`${index}:${hit.path}`}>{hit.path}</li>)}</ul>
              </div>}
              {preview && preview.conflicts.foreign_conflicts.length > 0 && <div className="conflict-box">
                <p>{t("installWizard.preflight_conflicts")}</p>
                <ul>{preview.conflicts.foreign_conflicts.map((conflict, index) => <li key={`${index}:${conflict.path}`}>
                  <code>{conflict.path}</code> — {conflict.owner_mod_id === null
                    ? t("installWizard.preflight_untracked")
                    : t("installWizard.owned_by", { name: conflict.owner_name })}
                </li>)}</ul>
              </div>}
              {preview?.conflicts.self_update && <p>{t("installWizard.preflight_update", {
                name: preview.conflicts.self_update.existing_name,
              })}</p>}

              <div className="comp-row" style={{ marginTop: 10 }}>
                <span className="name">{t("installWizard.scan_label")}</span>
                {scanning && <span className="comp-checking">{t("installWizard.scan_running")}</span>}
                {!scanning && scanResult === "Clean" && (
                  <span className="comp-ok">{t("installWizard.scan_clean")}</span>
                )}
                {!scanning && scanResult !== null && typeof scanResult === "object" && "ThreatDetected" in scanResult && (
                  <span className="comp-missing">{t("installWizard.scan_threat")}</span>
                )}
                {!scanning && scanResult !== null && typeof scanResult === "object" && "Unavailable" in scanResult && (
                  <span className="comp-checking">{t("installWizard.scan_unavailable")}</span>
                )}
              </div>
              {!scanning && scanResult !== null && typeof scanResult === "object" && "ThreatDetected" in scanResult && (
                <p className="error" style={{ marginTop: 6 }}>
                  {t("installWizard.scan_threat_body", {
                    details: scanResult.ThreatDetected.details ?? t("installWizard.scan_no_details"),
                  })}
                </p>
              )}
            </>
          )}

          {step === "installing" && <p>{t("installWizard.installing")}</p>}

          {step === "done" && outcome && "Success" in outcome && (
            <>
              <p>
                {t("installWizard.success", { count: outcome.Success.files_written })}
              </p>
              {plan && typeof plan.format === "object" && "AddOnPack" in plan.format && (
                <p>{t("manualDlc.install_notice")}</p>
              )}
            </>
          )}

          {step === "done" && outcome && "ProtectedFileBlocked" in outcome && (
            <div className="conflict-box">
              <div className="head">{t("installWizard.protected_head")}</div>
              <p>{t("installWizard.protected_body")}</p>
              {outcome.ProtectedFileBlocked.map((p, i) => (
                <p key={i} className="mono" style={{ fontSize: 11 }}>
                  {p}
                </p>
              ))}
            </div>
          )}

          {step === "done" && outcome && "RequiresOverride" in outcome && (
            <div className="conflict-box">
              <div className="head">{t("installWizard.override_head")}</div>
              <p>{t("installWizard.override_body")}</p>
              {outcome.RequiresOverride.foreign_conflicts.map((c, i) => (
                <p key={i} className="mono" style={{ fontSize: 11 }}>
                  {c.path} — {t("installWizard.owned_by", { name: c.owner_name })}
                </p>
              ))}
            </div>
          )}

          {step === "error" && <p className="error">{error}</p>}
        </div>

        <div className="modal-foot">
          {step === "review" && (
            <>
              <button className="btn-ghost" type="button" onClick={handleClose}>
                {t("installWizard.cancel")}
              </button>
              <button className="btn-primary" type="button" disabled={blocked} onClick={() => runInstall(false)}>
                {t("installWizard.install")}
              </button>
            </>
          )}
          {step === "done" && outcome && "RequiresOverride" in outcome && (
            <>
              <button className="btn-ghost" type="button" onClick={handleClose}>
                {t("installWizard.cancel")}
              </button>
              <button className="btn-primary" type="button" onClick={() => runInstall(true)}>
                {t("installWizard.override_and_install")}
              </button>
            </>
          )}
          {(step === "done" || step === "error") &&
            !(outcome && "RequiresOverride" in outcome) && (
              <button className="btn-primary" type="button" onClick={handleClose}>
                {t("installWizard.close")}
              </button>
            )}
          {(step === "pick" || step === "analyzing" || step === "installing") && (
            <button className="btn-ghost" type="button" disabled={step === "installing"} onClick={handleClose}>
              {t("installWizard.cancel")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
