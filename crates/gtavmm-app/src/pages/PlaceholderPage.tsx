import { Icon } from "../components/IconSprite";
import { useTranslation } from "react-i18next";

// OpenAI-Codex: Distinguish temporarily unsupported pages from unfinished ports.
export function PlaceholderPage({
  title,
  comingSoon = false,
}: {
  title: string;
  comingSoon?: boolean;
}) {
  const { t } = useTranslation();

  return (
    <section className="view" data-shown="true">
      <div className="page-head">
        <div>
          <h1 className="page-title">{title}</h1>
        </div>
      </div>
      <div className="panel">
        <div className="empty-state">
          <span className="glyph">
            <Icon name="bar-chart" />
          </span>
          <h3>{comingSoon ? t("nav.coming_soon") : "Not ported yet"}</h3>
          {!comingSoon && (
            <p>
              This workspace exists in the HTML design mockup but hasn't been rebuilt in the
              real React app yet — see <span className="mono">16-UI-Page-Structure.md</span> for
              the plan.
            </p>
          )}
        </div>
      </div>
    </section>
  );
}
