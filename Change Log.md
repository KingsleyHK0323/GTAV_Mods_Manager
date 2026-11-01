<!-- OpenAI-Codex -->
<!-- OpenAI-Codex_Responsible: Website -->
<!-- Anthropic-Claude_Code_Responsible: Website -->
# Change Log

## Unreleased

- Align the GitHub Pages origin and repository links with the current owner, and verify complete social preview image URLs in SEO checks.
- Refresh bilingual guidance on supported modes, safety, privacy, downloads, DLL translation, and desktop app licensing.
- Clarify that the core rejects executable file targets during installation.
- Add paired Traditional Chinese and English documentation for downloads, privacy, supported modes, safety, and licensing.
- Replace the "language variants" wording with the app's actual feature, optional AI-assisted mod translation (INI/XML drafts and .NET DLL strings), on the home pages, landing sections, FAQ, and quick start. The former Language Changes guide is now "Mod Translation"; its route slug is unchanged.
- Label the landing-page version as the website version and localize it; use "Disabled" instead of "Paused" in the English sample list.
- Fix the English demo details sentence for multi-language samples.
- Update the sidebar label and README wording to match.

## 0.2.0 — 2026-10-27

- Add a bilingual in-memory activity summary to the interactive demo for status changes and sample additions.
- Add separate history clearing and demo reset behavior, and document the session-only history.
- Clarify session-only changes and reset behavior, and add a clear-search action when filters return no sample items.
- Update the website version to `0.2.0`.

## 0.1.2 — 2026-10-27

- Move the component signature comments into Astro frontmatter so the compiler can parse component props.
- Update the website version to `0.1.2`.

## 0.1.1 — 2026-10-27

- Fix the English mod installation guide's MDX comment so the documentation build succeeds.
- Update the website version to `0.1.1`.

## 0.1.0 — 2026-10-26

- Restore the Starlight docs collection loader and schema required to generate the documentation routes.
- Configure the GitHub Pages project base path, localized sitemap, canonical URLs, language alternates, and page-specific social metadata.
- Correct internal page and image URLs for the project subpath and localize the Traditional Chinese home-page description.
- Add a GitHub Actions workflow to audit, build, verify SEO output, and deploy after the repository Pages source is set to GitHub Actions.

## 0.0.2 — 2026-10-26

- Fix the Starlight logo asset path so Astro can resolve the supplied JPG during the static build.
- Clarify Kingsley's copyright and the CC BY-NC-SA 4.0 scope for all website content, including supplied images.

## 0.0.1 — 2026-10-26

- Add the Astro and Starlight website foundation with Traditional Chinese and English routes.
- Add a safe, browser-only sample demo, product landing page, and starter documentation.
- Add the owner-provided app icon and social preview image.
- Add a GitHub Actions hosted build workflow.
- Pin Astro 7.4.2 and Starlight 0.42.7 for the website toolchain.
- Add Discord community and GitHub project links to the site header and home page.
- Use the project-provided JPG assets for the site icon and social preview.
