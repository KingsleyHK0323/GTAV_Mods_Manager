// OpenAI-Codex_release_impl: Attach the verified original installers to the existing beta release.
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { REPOSITORY, TAG, BUILD_SHA, RUN_ID, ARTIFACT_ID, verifyProvenance, verifyInstallerDigests } from './publish-beta-recovery.mjs';
import { createGithubApi, collectPages, collectExpectedAssets, validateRelease, reconcileDraftAssets } from './prepare-beta-release.mjs';

export const RELEASE_ID = 445702174;
export function checkRelease(release) {
  validateRelease(release, RELEASE_ID, TAG);
}

async function verifySourceAndRelease(api) {
  const prefix = '/repos/' + REPOSITORY;
  const run = await api.json(prefix + '/actions/runs/' + RUN_ID);
  const jobs = await collectPages(async (page) => (await api.json(prefix + '/actions/runs/' + RUN_ID + '/jobs?per_page=100&page=' + page)).jobs);
  const artifact = await api.json(prefix + '/actions/artifacts/' + ARTIFACT_ID);
  verifyProvenance(run, jobs, artifact);
  await api.verifyTag();
  checkRelease(await api.getRelease(RELEASE_ID));
  return collectPages((page) => api.listAssets(RELEASE_ID, page));
}

async function main() {
  if (process.env.GH_REPOSITORY !== REPOSITORY || !process.env.GH_TOKEN) throw new Error('Unexpected repository or missing token.');
  const api = createGithubApi({ repository: REPOSITORY, token: process.env.GH_TOKEN, tag: TAG, commit: BUILD_SHA });
  await verifySourceAndRelease(api);
  if (process.argv[2] === 'preflight') return;
  if (process.argv[2] !== 'upload' || !path.isAbsolute(process.env.RUNNER_TEMP || '')) throw new Error('Invalid attachment phase or runner temp.');
  const assets = await collectExpectedAssets(path.join(process.env.RUNNER_TEMP, 'release-assets'));
  verifyInstallerDigests(assets);
  await verifySourceAndRelease(api);
  const result = await reconcileDraftAssets({ releaseId: RELEASE_ID, tag: TAG, expectedAssets: assets, api });
  checkRelease(await api.getRelease(RELEASE_ID));
  await api.verifyTag();
  console.log('Verified existing public beta release ID: ' + result.releaseId);
  for (const asset of assets) console.log(asset.sha256 + '  ' + asset.name);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error('Beta attachment failed:', error.message); process.exitCode = 1; });
}
