import { ModWorkspace } from "../components/ModWorkspace";
import { countByStatus } from "../lib/modStats";

/** OpenAI-Codex: Enable batch state controls on the GTA V Legacy LSPDFR page only. */
export function LegacyLspdfrPage() {
  return (
    <ModWorkspace
      pageMode="legacy-lspdfr"
      bulkStateActions
      titleKey="legacyLspdfr.title"
      subtitleKey="legacyLspdfr.subtitle"
      categories={["callouts", "eup-peds", "vehicles", "other"]}
      toolsVariant="framework"
      badges={["lspdfr"]}
      banner={{ tone: "info", icon: "info", key: "legacyLspdfr.known_limitation" }}
      stats={[
        { labelKey: "legacySp.stat_installed", value: ({ mods }) => mods.length },
        { labelKey: "legacySp.stat_active", tone: "accent", value: countByStatus("Active") },
        { labelKey: "legacySp.stat_disabled", value: countByStatus("Disabled") },
      ]}
    />
  );
}
