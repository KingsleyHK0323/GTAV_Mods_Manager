# Privacy Policy

_Last updated: 2026-10-27_

## Summary

GTAV Mods Manager is **offline-first**. It has no account system, first-party server,
or telemetry. Core mod-management operations use files on your device and a local
SQLite database. Optional AI features and user-requested update checks make network
requests described below. The app does not automatically scan or upload your game
files or mod database; content you explicitly select for AI translation can be sent
to the configured provider.

## What the Software stores, and where

The Software keeps tracked information — such as your detected game path,
installed-mod records, install/uninstall history, settings, and recycle bin — in a
local SQLite database and local folders under your OS's standard per-user
application-data directory (e.g. `%APPDATA%\SpaceSquare\GTAVModsManager` on Windows).
The app does not upload this database as part of the network features described below.
Uninstalling the Software and deleting that folder removes this local app data.

## Network activity in the current version

Install, uninstall, conflict detection, and backup operate on local files and the
local database; these core operations do not make network requests.

The update check runs only when you request it from Settings or the CLI. It makes an
unauthenticated request to the public GitHub Releases API to read the latest release
version and links. It does not download or install an update and does not send your
game path or mod list. Like any internet request, GitHub receives connection
metadata such as your IP address.

## Optional AI features

AI is optional and disabled by default. You must enable it and invoke a feature for
text to be sent. The app supports Ollama running at `localhost`, Ollama Cloud, OpenAI,
Anthropic, DeepSeek, OpenRouter, Google AI, NVIDIA, and a compatible custom cloud
endpoint. For cloud providers, the app sends requests directly to the provider you
selected; this project does not relay those requests. Cloud requests may incur fees
under the provider's terms. API keys are stored in the operating system's credential
store, not in the app's SQLite database or plaintext settings.

When you request a diagnosis, the desktop app sends only the text you paste into its
diagnosis field; it does not scan files. The CLI can instead read a file you specify
or text from standard input. Before diagnosis text is sent, the app replaces the
current user's home-directory path with `<home>`. This is limited de-identification,
not full anonymization; other identifying or sensitive text may remain.

For INI/XML translation, the selected file's text is sent to the configured provider.
For DLL translation, extracted candidate strings from the selected DLL are sent; the
DLL binary itself is not sent. Translation input is not de-identified and may contain
paths or other sensitive text. Review the selected content before using a provider.
With Ollama Local, the app sends the request to the Ollama service at
`localhost:11434`; this describes the app's connection and not any separate behavior
of software you install or run.

## Crash report drafts

The CLI can print a pre-filled GitHub Issue draft containing the app version, OS, and
error text. The draft replaces occurrences of the current home-directory path with
`<home>`, but this does not remove other identifying or sensitive details. Creating
or printing the URL makes no network request. Opening the URL sends its pre-filled
title and report text to GitHub to display the draft; the issue is not published
unless you review it and submit it on GitHub. The desktop app does not automatically
submit crash reports.

## Planned features

Mod-source integration, such as checking mod websites for update information, is not
implemented. If a future feature changes these data flows, this policy will be
updated before that feature is described as available.

## Children's privacy

The Software does not perform age verification (see [TERMS.md](TERMS.md) §9). If a
user chooses to use an AI provider or open a crash report draft, the data handling
described above applies, including the selected provider's or GitHub's own terms and
privacy policy.

## Changes to this policy

If a future version changes these data flows, this file will be updated to describe
the current behavior and its "Last updated" date will change.

## Contact

Questions about this policy: open an issue on
[this repository](https://github.com/SpaceSquare640/GTAV_Mods_Manager/issues).

— Kingsley

<!-- OpenAI-Codex -->
<!-- OpenAI-Codex_Responsible: Source_Code -->
