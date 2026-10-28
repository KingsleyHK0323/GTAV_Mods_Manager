# Plan: On-Demand GitHub Release Metadata for Installed Mods

**Status:** Approved for implementation on 2026-10-27\
**Project:** `Source_Code`\
**Planning date:** 2026-10-27\
**Plan author:** OpenAI-Codex\
**Responsible area:** Source_Code

## Goal

Let a user manually look up the latest public GitHub release associated with a mod's source link from the mod details drawer. Show release metadata so the user can compare it themselves. The app does not know the installed mod version, so the UI must not claim that an update is available.

## Product and network decisions

- Accept only HTTPS GitHub repository links. Parse and validate the owner and repository before constructing a request to the fixed `https://api.github.com` host. Never fetch an arbitrary URL from the source-link field.
- Make a single request only after the user activates the lookup button. Do not query when a drawer opens, poll, retry automatically, download a release, or install anything.
- Use GitHub's `GET /repos/{owner}/{repo}/releases/latest` endpoint. It returns the most recent published full release, excluding drafts and prereleases; GitHub selects it by `created_at`. Display `tag_name`, the release `name` when present, `published_at` when present, and a verified GitHub `html_url`. Render release text as text and validate the release link before making it clickable.
- Public repository requests can be unauthenticated. Send only the repository owner/name, a fixed application User-Agent, and standard API headers. Do not send mod names, notes, install paths, local files, or credentials.
- Treat HTTP 404 as ambiguous: the repository may be private/inaccessible or may have no public latest release. Show that accurately instead of claiming the repository has no releases. Handle 403/429 as a rate-limit or access error, surface `Retry-After`/reset guidance when available, and do not retry automatically.
- Keep the feature GitHub-only. Other providers show a localized unsupported-source state without making a request.
- Do not add a database field or dependency. Reuse the existing saved source link and `ureq` dependency.

## Scope and likely files

- New `crates/gtavmm-core/src/mod_source_check/mod.rs`; export the module from `crates/gtavmm-core/src/lib.rs`.
- Add a thin Tauri command in `crates/gtavmm-app/src-tauri/src/commands.rs` and register it in `crates/gtavmm-app/src-tauri/src/lib.rs`.
- Add the user-triggered lookup and accessible loading, result, unsupported-source, and error states in `crates/gtavmm-app/src/components/ModDetailDrawer.tsx`.
- Add any shared response type to `crates/gtavmm-app/src/types.ts`; add matching copy to `crates/gtavmm-app/src/i18n/locales/en.json` and `crates/gtavmm-app/src/i18n/locales/zh-TW.json`.
- Update `PRIVACY.md` before describing the feature as available, including the destination and data sent. Update `Change Log.md`. Review `README.md` and change it only if its existing feature claims need correction or expansion.
- No schema migration, lockfile change, or automatic-release workflow is planned.

## Implementation steps

1. **Add core URL and response tests first.** Cover accepted HTTPS GitHub repository links and rejection of other hosts, HTTP, user-info, ports, encoded separators, query/fragment tricks, malformed owner/repository segments, and empty values. Cover release JSON with optional fields, invalid JSON, and safe handling of returned release URLs. Run the targeted tests and confirm they fail for the missing behavior.
2. **Implement the core lookup boundary.** Add a small typed result for a release, no public latest release, and unsupported source. Validate the link before building the fixed API URL. Parse only the fields the UI needs. Keep the HTTP request behind a private injectable seam so tests can provide canned status/body/header values without contacting GitHub. Cover 200, 404, 403/429, server errors, malformed responses, and the exact fixed API host. Do not add a general URL-fetching interface.
3. **Expose the lookup through Tauri.** Add a thin wrapper that delegates to `gtavmm-core`, converts errors consistently, and registers the command. Verify unsupported or malformed input is rejected before any network call.
4. **Connect the drawer UI.** Invoke the command only from an explicit button action using the currently visible source-link value. Disable the button while a request is in flight. Present the latest tag/name/date and release link; distinguish unsupported links, no publicly accessible latest release, rate limits, and other failures. Keep the current mod metadata and form state intact. Use the existing opener/link conventions and accessible live status text.
5. **Update documentation and localization.** Add equivalent English and Traditional Chinese strings. Update the privacy network-activity and planned-feature wording, its last-updated date, and the Unreleased changelog. Preserve existing README content unless it makes an inaccurate claim about mod-source checks.
6. **Review the complete diff.** Check the fixed-host boundary, URL allowlist, returned-link validation, response text rendering, manual-only trigger, and privacy claims. Ensure each changed file has the required `OpenAI-Codex` / `OpenAI-Codex_Responsible: Source_Code` signature in a format valid for that file.

## Verification after implementation approval

- Run targeted `gtavmm-core` tests for the new module, then `cargo test --workspace --offline`.
- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings`, and `git diff --check`.
- Run `npm run build` from `crates/gtavmm-app` to validate TypeScript, localization imports, and the frontend bundle.
- Tests use canned responses only; do not call the live GitHub API during verification. No dependency installation, commit, or push is included in this plan.
- After implementation, perform an independent security-focused diff review if a suitable agent is available, then append the implementation, test, review, and any anomalies to the authorized operation log.

## Risks and limits

- Without an installed-version field, the app cannot determine whether a release is newer than the installed mod.
- Unauthenticated GitHub API requests for public data are limited to 60 per hour per originating IP; users behind the same public IP share that limit. The feature therefore remains manual-only, with no polling or automatic retries.
- A 404 does not distinguish a private/inaccessible repository from a repository without a public latest release.
- GitHub can be unavailable or the user can be offline; these failures must remain non-fatal and must not affect local mod-management operations.

## Skills considered

- Planning: `writing-plans` and `using-superpowers`.
- Implementation: `test-driven-development`, `Karpathy Guidelines`, `frontend-design`, `vercel-react-best-practices`, `security-review`, `requesting-code-review`, and `verification-before-completion`.
- The catalog has no exact `security-audit-skill` entry; `security-review` is the available security-review alternative. Browser automation is not included because this is a local Tauri UI and the project has no frontend test script. No new skills or services will be installed or connected.

## Official references consulted

- [GitHub REST API: Get the latest release](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)
- [GitHub REST API rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)
- [GitHub REST API best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)
- [GitHub REST API troubleshooting](https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api)

<!-- OpenAI-Codex -->
<!-- OpenAI-Codex_Responsible: Source_Code -->
