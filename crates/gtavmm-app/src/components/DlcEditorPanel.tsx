// OpenAI-Codex
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { pickFile, pickSaveFile } from "../lib/pickers";
import type { DlcAiPreview, DlcAiResult, DlcEditorAnalysis, DlcEditorDocument } from "../types";

/** A memory-only draft; switching tool tabs keeps this component mounted. */
export function DlcEditorPanel() {
  const { t } = useTranslation();
  const [document, setDocument] = useState<DlcEditorDocument | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState<"open" | "validate" | "save" | "ai" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [analysis, setAnalysis] = useState<{
    sourcePath: string;
    sourceHash: string;
    draft: string;
    result: DlcEditorAnalysis;
  } | null>(null);
  const generation = useRef(0);
  // OpenAI-Codex: AI results never replace a draft without a separate user action.
  const mounted = useRef(true);
  const aiInFlight = useRef(false);
  const [instruction, setInstruction] = useState("");
  const [aiPreview, setAiPreview] = useState<DlcAiPreview | null>(null);
  const [aiResult, setAiResult] = useState<DlcAiResult | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current += 1; };
  }, []);
  const validated = Boolean(document && analysis &&
    analysis.sourcePath === document.source_path &&
    analysis.sourceHash === document.source_hash && analysis.draft === draft);

  async function openDocument() {
    setBusy("open");
    setError(null);
    try {
      const path = await pickFile(["xml"], t("dlclistEditor.open_title"));
      if (!path) return;
      if (document && draft !== document.content && !window.confirm(t("dlclistEditor.discard_confirm"))) return;
      const request = ++generation.current;
      const next = await invoke<DlcEditorDocument>("read_dlclist_document", { path });
      if (request !== generation.current) return;
      setDocument(next);
      setDraft(next.content);
      setAnalysis(null);
      setSavedPath(null);
      setAiPreview(null);
      setAiResult(null);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  }

  function editDraft(content: string) {
    generation.current += 1;
    // Textarea input normalizes newlines; retain a uniform source CRLF convention.
    const source = document?.content ?? "";
    const crlf = source.includes("\r\n") && !source.replace(/\r\n/g, "").includes("\n");
    setDraft(crlf ? content.replace(/\r?\n/g, "\r\n") : content);
    setAnalysis(null);
    setSavedPath(null);
    setError(null);
    setAiPreview(null);
    setAiResult(null);
  }

  function editInstruction(value: string) {
    generation.current += 1;
    setInstruction(value);
    setAiPreview(null);
    setAiResult(null);
    setError(null);
  }

  async function requestAi(action: "preview" | "send" | "apply") {
    if (!document || busy || aiInFlight.current) return;
    if (action !== "preview" && !aiPreview) return;
    if (action === "apply" && !aiResult) return;
    aiInFlight.current = true;
    const request = ++generation.current;
    const content = draft;
    const currentPreview = aiPreview;
    const currentResult = aiResult;
    const current = () => mounted.current && request === generation.current;
    setBusy("ai");
    setError(null);
    try {
      if (action === "send") {
        setAiResult(null);
        const result = await invoke<DlcAiResult>("suggest_dlclist_ai", {
          content, instruction, expected: currentPreview,
        });
        if (current()) setAiResult(result);
      } else {
        const preview = await invoke<DlcAiPreview>("prepare_dlclist_ai", { content, instruction });
        if (!current()) return;
        if (action === "preview") {
          setAiPreview(preview);
          setAiResult(null);
        } else if (currentPreview && currentResult) {
          // Recheck the public settings snapshot before applying a pending result.
          if (JSON.stringify(preview) !== JSON.stringify(currentPreview)) {
            setAiPreview(null);
            setAiResult(null);
            throw new Error(t("dlclistEditor.ai_changed"));
          }
          setDraft(currentResult.content);
          setAnalysis(null);
          setSavedPath(null);
          setAiPreview(null);
          setAiResult(null);
          generation.current += 1;
        }
      }
    } catch (reason) {
      if (current()) {
        setError(String(reason));
        if (action === "send") setAiPreview(null);
      }
    } finally {
      aiInFlight.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  async function validateDraft() {
    if (!document) return;
    const current = document;
    const content = draft;
    const request = ++generation.current;
    setBusy("validate");
    setError(null);
    setAnalysis(null);
    try {
      const result = await invoke<DlcEditorAnalysis>("validate_dlclist_draft", {
        original: current.content, draft: content,
      });
      if (request === generation.current) {
        setAnalysis({ sourcePath: current.source_path, sourceHash: current.source_hash, draft: content, result });
      }
    } catch (reason) {
      if (request === generation.current) setError(String(reason));
    } finally {
      setBusy(null);
    }
  }

  async function saveCopy() {
    if (!document || !validated) return;
    const current = document;
    const content = draft;
    setBusy("save");
    setError(null);
    setSavedPath(null);
    try {
      const outputPath = await pickSaveFile(
        current.source_path.replace(/\.xml$/i, ".edited.xml"), ["xml"], t("dlclistEditor.save_title"),
      );
      if (!outputPath) return;
      const result = await invoke<string>("save_dlclist_copy", {
        sourcePath: current.source_path, expectedSourceHash: current.source_hash,
        outputPath, content,
      });
      setSavedPath(result);
      // Keep the original source snapshot; saving a copy never edits that file.
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  }

  const result = validated ? analysis?.result : null;
  const textStyle = { width: "100%", minHeight: 240, fontFamily: "monospace", resize: "vertical" as const };
  return (
    <section className="panel" aria-label={t("dlclistEditor.title")}>
      <div className="panel-head"><h2>{t("dlclistEditor.title")}</h2></div>
      <div style={{ padding: "16px 20px", overflowWrap: "anywhere" }}>
        <p>{t("dlclistEditor.description")}</p>
        <p>{t("dlclistEditor.limits")}</p>
        <button type="button" className="btn-primary" disabled={busy !== null} onClick={openDocument}>
          {t("dlclistEditor.open")}
        </button>
        <div aria-live="polite">
          {busy && <p role="status">{t(`dlclistEditor.busy_${busy}`)}</p>}
          {error && <p className="error" role="alert">{error}</p>}
          {savedPath && <p role="status">{t("dlclistEditor.saved", { path: savedPath })}</p>}
        </div>
        {!document && <p>{t("dlclistEditor.empty")}</p>}
        {document && (
          <>
            <p>{t("dlclistEditor.source_path", { path: document.source_path })}</p>
            <label htmlFor="dlclist-original">{t("dlclistEditor.original")}</label>
            <textarea id="dlclist-original" readOnly value={document.content} style={textStyle} spellCheck={false} />
            <label htmlFor="dlclist-draft">{t("dlclistEditor.draft")}</label>
            <textarea id="dlclist-draft" value={draft} onChange={(event) => editDraft(event.target.value)}
              disabled={busy === "open" || busy === "save"} style={textStyle} spellCheck={false} />
            <div style={{ display: "flex", flexWrap: "wrap", gap: 12, margin: "12px 0" }}>
              <button type="button" className="btn-primary" disabled={busy !== null} onClick={validateDraft}>
                {t("dlclistEditor.validate")}
              </button>
              <button type="button" className="btn-primary" disabled={busy !== null || !validated} onClick={saveCopy}>
                {t("dlclistEditor.save")}
              </button>
            </div>
            <fieldset disabled={busy !== null}>
              <legend>{t("dlclistEditor.ai_title")}</legend>
              <p>{t("dlclistEditor.ai_privacy")}</p>
              <label htmlFor="dlclist-ai-instruction">{t("dlclistEditor.ai_instruction")}</label>
              <textarea id="dlclist-ai-instruction" value={instruction}
                onChange={(event) => editInstruction(event.target.value)} style={{ width: "100%", minHeight: 80 }} />
              <button type="button" className="btn-primary" disabled={!instruction.trim()}
                onClick={() => requestAi("preview")}>{t("dlclistEditor.ai_preview")}</button>
              {aiPreview && <div>
                <p>{t("dlclistEditor.ai_destination", { provider: aiPreview.provider.provider,
                  model: aiPreview.provider.model, endpoint: aiPreview.provider.endpoint })}</p>
                <label htmlFor="dlclist-ai-system">{t("dlclistEditor.ai_system")}</label>
                <textarea id="dlclist-ai-system" readOnly value={aiPreview.provider.system_prompt} style={textStyle} />
                <label htmlFor="dlclist-ai-prompt">{t("dlclistEditor.ai_prompt")}</label>
                <textarea id="dlclist-ai-prompt" readOnly value={aiPreview.user_prompt} style={textStyle} />
                <button type="button" className="btn-primary" disabled={aiResult !== null}
                  onClick={() => requestAi("send")}>{t("dlclistEditor.ai_send")}</button>
              </div>}
              {aiResult && <div>
                <label htmlFor="dlclist-ai-result">{t("dlclistEditor.ai_result")}</label>
                <textarea id="dlclist-ai-result" readOnly value={aiResult.content} style={textStyle} />
                <h3>{t("dlclistEditor.added")}</h3>
                <ul>{aiResult.analysis.diff.added.map((entry, index) => <li key={`${index}:${entry}`}><code>{entry}</code></li>)}</ul>
                <h3>{t("dlclistEditor.removed")}</h3>
                <ul>{aiResult.analysis.diff.removed.map((entry, index) => <li key={`${index}:${entry}`}><code>{entry}</code></li>)}</ul>
                {aiResult.analysis.diff.reordered && <p>{t("dlclistEditor.reordered")}</p>}
                {aiResult.analysis.validation.warnings.map((warning) => <p key={warning}>{t(`dlclistEditor.warning_${warning}`, warning)}</p>)}
                {aiResult.analysis.validation.duplicate_entries.length > 0 && <p>{t("dlclistEditor.duplicates", {
                  entries: aiResult.analysis.validation.duplicate_entries.join(", "),
                })}</p>}
                <button type="button" className="btn-primary" onClick={() => requestAi("apply")}>
                  {t("dlclistEditor.ai_apply")}</button>
              </div>}
            </fieldset>
            {!validated && <p>{t("dlclistEditor.validation_required")}</p>}
            {result && (
              <div aria-live="polite">
                <p>{t("dlclistEditor.valid", { count: result.validation.entries.length })}</p>
                {result.validation.warnings.length > 0 && (
                  <ul>{result.validation.warnings.map((warning) => <li key={warning}>{t(`dlclistEditor.warning_${warning}`, warning)}</li>)}</ul>
                )}
                {result.validation.duplicate_entries.length > 0 && (
                  <p>{t("dlclistEditor.duplicates", { entries: result.validation.duplicate_entries.join(", ") })}</p>
                )}
                <h3>{t("dlclistEditor.added")}</h3>
                {result.diff.added.length ? <ul>{result.diff.added.map((entry, index) => <li key={`${index}:${entry}`}><code>{entry}</code></li>)}</ul> : <p>{t("dlclistEditor.none")}</p>}
                <h3>{t("dlclistEditor.removed")}</h3>
                {result.diff.removed.length ? <ul>{result.diff.removed.map((entry, index) => <li key={`${index}:${entry}`}><code>{entry}</code></li>)}</ul> : <p>{t("dlclistEditor.none")}</p>}
                {result.diff.reordered && <p>{t("dlclistEditor.reordered")}</p>}
              </div>
            )}
          </>
        )}
      </div>
    </section>
  );
}
