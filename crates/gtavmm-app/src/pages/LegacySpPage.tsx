import { ModWorkspace } from "../components/ModWorkspace";
import { countByStatus, needsReview } from "../lib/modStats";

/** OpenAI-Codex: Enable batch state controls on the GTA V Legacy SP page only. */
export function LegacySpPage() {
  return (
    <ModWorkspace
      pageMode="legacy-sp"
      titleKey="legacySp.title"
      subtitleKey="legacySp.subtitle"
      showExcelExport
      bulkStateActions
      stats={[
        { labelKey: "legacySp.stat_installed", value: ({ mods }) => mods.length },
        { labelKey: "legacySp.stat_active", tone: "accent", value: countByStatus("Active") },
        { labelKey: "legacySp.stat_disabled", value: countByStatus("Disabled") },
        { labelKey: "modWorkspace.stat_needs_review", tone: "warn", value: needsReview },
      ]}
    />
  );
}
