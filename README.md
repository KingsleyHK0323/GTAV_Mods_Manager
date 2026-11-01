<!-- OpenAI-Codex -->
<!-- OpenAI-Codex_Responsible: Website -->
<!-- Anthropic-Claude_Code_Responsible: Website -->
# GTAV Mods Manager Website

Version `0.2.0` is the current Astro and Starlight website draft for GTAV Mods Manager. It brings the product overview, an interactive sample-data demo, and bilingual documentation into one responsive site.

> **Visit the website:** [Open the GTAV Mods Manager website](https://spacesquare640.github.io/GTAV_Mods_Manager/)

Copyright © Kingsley.

## What is included

- Traditional Chinese and English pages with Starlight search and navigation.
- A browser-only demo for filtering, selecting, and changing sample mod states, with a session activity summary. It does not access GTA V folders or persist demo changes or history.
- Bilingual product documentation for getting started, mod installation, mod translation, installed mod management, downloads, privacy, supported modes, safety, and licenses, including current mode restrictions and recovery and data-flow limits.
- Light and dark themes, system theme behavior, keyboard-accessible controls, and increased-contrast styles.
- GitHub Actions workflow that builds, verifies bilingual SEO output, and deploys the site from the website branch on GitHub-hosted runners.

## Versioning

The website uses Semantic Versioning (`MAJOR.MINOR.PATCH`). `MAJOR` marks incompatible changes, `MINOR` adds functionality or content compatibly, and `PATCH` covers backward-compatible fixes. The current version is `0.2.0`.

## Build automation

The workflow in `.github/workflows/site-build.yml` installs dependencies, audits the resolved tree for high and critical advisories, builds the site, and checks the generated bilingual SEO metadata and sitemap on GitHub-hosted Actions runners. Pull requests targeting `GTAV_Mods_Manager_Website` run the build and checks. Pushes to that branch also upload and deploy the Pages artifact; manual runs deploy only when dispatched from that branch. Set the repository Pages source to **GitHub Actions** before deployment. This source tree has no dependency lockfile, so the hosted job resolves a fresh dependency tree for each run. Local build and test commands are not run as part of this release.

## Search engines

Each localized page has a canonical URL, language alternate links, and page-specific Open Graph and Twitter metadata. The sitemap integration publishes a localized sitemap index. Pages remain indexable by default; the project site is served under `/GTAV_Mods_Manager/`, so this repository does not publish a `robots.txt` file at the domain root.

## Visual assets

The project owner supplied JPG versions of the app icon and social preview. The app icon is copied byte-for-byte to `src/assets/app-icon.jpg` for the Starlight logo and `public/assets/app-icon.jpg` for the browser icon. The social preview is copied to `public/assets/social-preview.jpg` for the home page hero and Open Graph/Twitter metadata. The original PNG files remain in the directory but are not used by the site.

## Licenses

- Website source code: [PolyForm Noncommercial License 1.0.0](./LICENSE.md).
- All website content, including text and visual assets such as the supplied app icon and social preview, is licensed under [Creative Commons Attribution-NonCommercial-ShareAlike 4.0 International](./LICENSE-CONTENT.md).
- Website copyright: Kingsley.
- The license text comes from the [official PolyForm plain-text license](https://polyformproject.org/licenses/noncommercial/1.0.0.txt) and [official Creative Commons legal code](https://creativecommons.org/licenses/by-nc-sa/4.0/legalcode.txt). A trailing HTML comment identifies the implementation signature without changing the license wording.

These terms make this project source-available under noncommercial licenses.

## Project links

- [GTAV Mods Manager repository](https://github.com/KingsleyHK0323/GTAV_Mods_Manager)
- [Changelog](./Change%20Log.md)
