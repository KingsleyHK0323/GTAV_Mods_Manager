// OpenAI-Codex
// OpenAI-Codex_manual_dlc_ui
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { ManualDlcTask, PageMode } from "../types";

/** Shared-root reminders; these never claim that the game's XML was changed. */
export function ManualDlcTasks({
  mode,
  refreshRevision,
}: {
  mode: PageMode;
  refreshRevision: number;
}) {
  const { t } = useTranslation();
  const [tasks, setTasks] = useState<ManualDlcTask[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retryRevision, setRetryRevision] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setTasks(null);
    setError(null);
    invoke<ManualDlcTask[]>("list_manual_dlc_tasks", { gamePath: null, mode })
      .then((result) => {
        if (!cancelled) setTasks(result);
      })
      .catch((reason) => {
        if (!cancelled) setError(String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [mode, refreshRevision, retryRevision]);

  return (
    <section className="panel" aria-label={t("manualDlc.title")}>
      <div className="panel-head">
        <h2>{t("manualDlc.title")}</h2>
        <button className="link-btn" type="button" onClick={() => setRetryRevision((n) => n + 1)}>
          {t("manualDlc.refresh")}
        </button>
      </div>
      <div style={{ padding: "16px 20px", overflowWrap: "anywhere" }}>
        <p>{t("manualDlc.description")}</p>
        <p>{t("manualDlc.shared_root")}</p>
        <div aria-live="polite" aria-busy={tasks === null && error === null}>
          {error && <p className="error" role="alert">{t("manualDlc.load_error", { error })}</p>}
          {tasks === null && error === null && <p>{t("manualDlc.loading")}</p>}
          {tasks?.length === 0 && <p>{t("manualDlc.empty")}</p>}
          {tasks && tasks.length > 0 && (
            <ul>
              {tasks.map((task) => {
                const recommendation = task.unresolved
                  ? "review"
                  : task.recommendation === "check_present" || task.recommendation === "check_removal"
                    ? task.recommendation
                    : "review";
                return (
                  <li key={task.id} style={{ marginBottom: 16 }}>
                    <h3>{task.pack_name}</h3>
                    <p>{t("manualDlc.root", { path: task.root_path })}</p>
                    <p>{t("manualDlc.operation", { operation: t(`manualDlc.operation_${task.operation}`, task.operation) })}</p>
                    <p><strong>{t(`manualDlc.${recommendation}`)}</strong></p>
                    {task.unresolved && <p>{t("manualDlc.unresolved")}</p>}
                    <p>{t("manualDlc.owners", { owners: task.owner_mod_ids.length > 0 ? task.owner_mod_ids.map((id) => `#${id}`).join(", ") : t("manualDlc.no_owners") })}</p>
                    <p>{t("manualDlc.item_label")}</p>
                    <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}><code>{task.item_xml}</code></pre>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </section>
  );
}
