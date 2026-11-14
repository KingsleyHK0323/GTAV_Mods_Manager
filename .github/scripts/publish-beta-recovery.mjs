// OpenAI-Codex_release_impl: Recover the reviewed beta from its original Windows artifact.
import path from 'node:path';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import {
  createGithubApi, collectPages, assertNoExistingRelease, collectExpectedAssets,
  reconcileDraftAssets, selectDraftRelease,
} from './prepare-beta-release.mjs';

export const REPOSITORY = 'KingsleyHK0323/GTAV_Mods_Manager';
export const TAG = 'v1.0.0-beta';
export const BUILD_SHA = '8190eb6fe27b8d91d21699c59a84c780e6d22464';
export const RUN_ID = 43600042920;
export const ARTIFACT_ID = 14115924634;
export const ZIP_SHA256 = 'c4048f45c1494ac268c1a3e751698facf721f406896a4fe87d6a9e9359e61073';

export function verifyProvenance(run, jobs, artifact) {
  if (!run || run.id !== RUN_ID || run.head_sha !== BUILD_SHA || run.head_branch !== TAG ||
      run.event !== 'push' || run.status !== 'completed') throw new Error('Original workflow run is not the reviewed build.');
  const required = ['Validate beta release tag and versions', 'test-gate / build-and-test (windows-latest)', 'build-windows'];
  if (!Array.isArray(jobs)) throw new Error('Original job list is unavailable.');
  for (const name of required) {
    const matches = jobs.filter((job) => job.name === name);
    if (matches.length !== 1 || matches[0].run_id !== RUN_ID || matches[0].conclusion !== 'success') {
      throw new Error('Required original validation or build job did not succeed: ' + name);
    }
  }
  if (!artifact || artifact.id !== ARTIFACT_ID || artifact.name !== 'installers-windows' || artifact.expired !== false ||
      artifact.digest !== 'sha256:' + ZIP_SHA256 || artifact.workflow_run?.id !== RUN_ID ||
      artifact.workflow_run?.head_sha !== BUILD_SHA) throw new Error('Original installer artifact identity does not match.');
}

export function verifyInstallerDigests(assets) {
  if (!Array.isArray(assets)) throw new Error('Invalid installer asset list.');
  const checksums = assets.filter((asset) => asset.name === 'SHA256SUMS-windows.txt');
  const installers = assets.filter((asset) => asset.name !== 'SHA256SUMS-windows.txt');
  const reviewed = {
    'GTAV Mods Manager_1.0.0_x64_en-US.msi': '903da42569003df651c54b207a4985c9ebaacf68172a1fe2a917ab820ffebd8d',
    'GTAV Mods Manager_1.0.0_x64-setup.exe': '33e6650c646c4f5f663bc24eea4f1dbd127174ad94b07f94b4d02dbe9a9fc930',
  };
  if (installers.length !== Object.keys(reviewed).length ||
      installers.some((asset) => reviewed[asset.name] !== asset.sha256)) {
    throw new Error('Installer names or SHA-256 differ from the independently verified build.');
  }
  if (checksums.length !== 1 || installers.length === 0) throw new Error('Windows installers or checksum file are missing.');
  const names = new Set();
  for (const installer of installers) {
    if (typeof installer.name !== 'string' || !/\.(msi|exe)$/i.test(installer.name) ||
        !Number.isSafeInteger(installer.size) || installer.size <= 0 ||
        !/^[a-f0-9]{64}$/.test(installer.sha256) || names.has(installer.name.toLowerCase())) {
      throw new Error('Invalid or duplicate Windows installer.');
    }
    names.add(installer.name.toLowerCase());
  }
  const data = Buffer.from(installers.map((asset) => asset.sha256 + '  ' + asset.name + '\n').join(''), 'utf8');
  const checksum = checksums[0];
  if (!Buffer.isBuffer(checksum.data) || !checksum.data.equals(data) || checksum.size !== data.length ||
      checksum.sha256 !== createHash('sha256').update(data).digest('hex')) {
    throw new Error('Windows checksum does not match the verified installers.');
  }
}

async function checkSource(api) {
  const prefix = '/repos/' + REPOSITORY;
  const run = await api.json(prefix + '/actions/runs/' + RUN_ID);
  const jobs = await collectPages(async (page) => (await api.json(prefix + '/actions/runs/' + RUN_ID + '/jobs?per_page=100&page=' + page)).jobs);
  const artifact = await api.json(prefix + '/actions/artifacts/' + ARTIFACT_ID);
  verifyProvenance(run, jobs, artifact);
  await api.verifyTag();
  assertNoExistingRelease(await collectPages(api.listReleases), TAG);
}

async function main() {
  if (process.env.GH_REPOSITORY !== REPOSITORY || !process.env.GH_TOKEN) throw new Error('Unexpected repository or missing token.');
  const api = createGithubApi({ repository: REPOSITORY, token: process.env.GH_TOKEN, tag: TAG, commit: BUILD_SHA });
  await checkSource(api);
  if (process.argv[2] === 'preflight') return;
  if (process.argv[2] !== 'publish' || !path.isAbsolute(process.env.RUNNER_TEMP || '')) throw new Error('Invalid recovery phase or runner temp.');
  const assets = await collectExpectedAssets(path.join(process.env.RUNNER_TEMP, 'release-assets'));
  verifyInstallerDigests(assets);
  await checkSource(api);
  const release = await api.createDraft();
  const selected = selectDraftRelease(await collectPages(api.listReleases), TAG);
  if (!selected || selected.id !== release.id) throw new Error('Created public release is missing or ambiguous.');
  const result = await reconcileDraftAssets({ releaseId: release.id, tag: TAG, expectedAssets: assets, api });
  const finalRelease = selectDraftRelease(await collectPages(api.listReleases), TAG);
  if (!finalRelease || finalRelease.id !== result.releaseId) throw new Error('Public release identity changed.');
  await api.verifyTag();
  console.log('Verified public beta release ID: ' + release.id);
  for (const asset of assets) console.log(asset.sha256 + '  ' + asset.name);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error('Beta recovery failed:', error.message); process.exitCode = 1; });
}
