<!-- OpenAI-Codex_Responsible: Website -->
# GTAV Mods Manager Website

Version `0.0.1` is the first Astro and Starlight website draft for GTAV Mods Manager. It brings the product overview, an interactive sample-data demo, and bilingual documentation into one responsive site.

## What is included

- Traditional Chinese and English pages with Starlight search and navigation.
- A browser-only demo for filtering, selecting, and changing sample mod states. It does not access GTA V folders or persist demo changes.
- Product documentation for getting started, mod installation, language changes, and installed mod management.
- Light and dark themes, system theme behavior, keyboard-accessible controls, and increased-contrast styles.
- GitHub Actions workflow that builds the site on a GitHub-hosted runner.

## Versioning

The website version follows the `x.x.x` version assigned to each website commit and push. The initial version is `0.0.1`.

## Build automation

The workflow in `.github/workflows/site-build.yml` installs dependencies, audits the resolved tree for high and critical advisories, and builds on GitHub-hosted Actions runners for pushes, pull requests, and manual dispatch. This initial source tree has no dependency lockfile; the hosted job resolves a fresh dependency tree for each run. No local build or test result is claimed for this draft.

## Visual assets

The project owner supplied JPG versions of the app icon and social preview; both are copied byte-for-byte into `public/assets/`. The app icon is used by the site header and browser icon. The social preview is used for the home page hero and Open Graph/Twitter metadata. The original PNG files remain in the directory but are not used by the site.

## Licenses

- Website source code: [PolyForm Noncommercial License 1.0.0](./LICENSE.md).
- Website text and original visual content: [Creative Commons Attribution-NonCommercial-ShareAlike 4.0 International](./LICENSE-CONTENT.md).
- The supplied app icon and social preview remain credited to their original owner, Kingsley; the content license does not override rights in those supplied assets.
- The license text comes from the [official PolyForm plain-text license](https://polyformproject.org/licenses/noncommercial/1.0.0.txt) and [official Creative Commons legal code](https://creativecommons.org/licenses/by-nc-sa/4.0/legalcode.txt). A trailing HTML comment identifies the implementation signature without changing the license wording.

These terms make this project source-available under noncommercial licenses.

## Project links

- [GTAV Mods Manager repository](https://github.com/SpaceSquare640/GTAV_Mods_Manager)
- [Changelog](./Change%20Log.md)
