// OpenAI-Codex_canonical_attachment_impl: Complete the existing beta without replacing its MSI.
import path from 'node:path';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import {
  REPOSITORY, TAG, BUILD_SHA, RUN_ID, ARTIFACT_ID, verifyProvenance, verifyInstallerDigests,
} from './publish-beta-recovery.mjs';
import { RELEASE_ID } from './publish-beta-attachment.mjs';
import {
  createGithubApi, collectPages, collectExpectedAssets, planAssetChanges,
  validateRelease, verifyAssetContent, reconcileDraftAssets,
} from './prepare-beta-release.mjs';

const MSI_ID = 749380433;
const MSI_NAME = 'GTAV.Mods.Manager_1.0.0_x64_en-US.msi';
const EXE_NAME = 'GTAV.Mods.Manager_1.0.0_x64-setup.exe';
const CHECKSUM_NAME = 'SHA256SUMS-windows.txt';
const ORIGINAL_MSI = 'GTAV Mods Manager_1.0.0_x64_en-US.msi';
const ORIGINAL_EXE = 'GTAV Mods Manager_1.0.0_x64-setup.exe';
const MSI_SIZE = 8593408;
const EXE_SIZE = 6397736;
const MSI_SHA256 = '903da42569003df651c54b207a4985c9ebaacf68172a1fe2a917ab820ffebd8d';
const EXE_SHA256 = '33e6650c646c4f5f663bc24eea4f1dbd127174ad94b07f94b4d02dbe9a9fc930';

function requireMsiIdentity(assets) {
  if (!Array.isArray(assets)) throw new Error('Release asset list is unavailable.');
  const matches = assets.filter((asset) => asset?.name === MSI_NAME);
  if (matches.length !== 1 || matches[0].id !== MSI_ID ||
      matches[0].size !== MSI_SIZE || matches[0].state !== 'uploaded') {
    throw new Error('The existing MSI identity or size does not match.');
  }
  return matches[0];
}

async function listAllAssets(api) {
  return collectPages((page) => api.listAssets(RELEASE_ID, page));
}

function originalInstaller(assets, name, size, digest) {
  const matches = assets.filter((asset) => asset.name === name);
  if (matches.length !== 1 || matches[0].size !== size || matches[0].sha256 !== digest ||
      typeof matches[0].path !== 'string' || !path.isAbsolute(matches[0].path)) {
    throw new Error('Original installer identity, size, digest or path does not match.');
  }
  return matches[0];
}

export function canonicalAssets(originalAssets) {
  verifyInstallerDigests(originalAssets);
  const msi = originalInstaller(originalAssets, ORIGINAL_MSI, MSI_SIZE, MSI_SHA256);
  const exe = originalInstaller(originalAssets, ORIGINAL_EXE, EXE_SIZE, EXE_SHA256);
  if (msi.path === exe.path) throw new Error('Original installer paths must differ.');
  const installers = [
    { name: MSI_NAME, path: msi.path, size: msi.size, sha256: msi.sha256 },
    { name: EXE_NAME, path: exe.path, size: exe.size, sha256: exe.sha256 },
  ];
  const data = Buffer.from(installers.map((asset) => asset.sha256 + '  ' + asset.name + '\n').join(''), 'utf8');
  const expected = [...installers, {
    name: CHECKSUM_NAME, data, size: data.length,
    sha256: createHash('sha256').update(data).digest('hex'),
  }];
  planAssetChanges(expected, []);
  return expected;
}

async function verifyCurrentAssets(api, expected, requireComplete = false) {
  validateRelease(await api.getRelease(RELEASE_ID), RELEASE_ID, TAG);
  await api.verifyTag();
  const current = await listAllAssets(api);
  requireMsiIdentity(current);
  const plan = planAssetChanges(expected, current);
  if (requireComplete && plan.missing.length !== 0) throw new Error('A required release asset is missing.');
  for (const pair of plan.existing) {
    await verifyAssetContent(pair.expected, pair.actual, api.getAssetDigest);
  }
  return plan;
}

export async function attachCanonical(api, originalAssets) {
  const expected = canonicalAssets(originalAssets);
  await verifyCurrentAssets(api, expected);
  const guardedApi = {
    ...api,
    listAssets: async (id, page) => {
      if (id !== RELEASE_ID) throw new Error('Unexpected release ID.');
      const assets = await api.listAssets(id, page);
      if (!Array.isArray(assets)) throw new Error('Release asset list is unavailable.');
      for (const asset of assets) {
        if (asset?.name === MSI_NAME && (asset.id !== MSI_ID || asset.size !== MSI_SIZE)) {
          throw new Error('The existing MSI identity or size changed.');
        }
      }
      return assets;
    },
    uploadAsset: async (id, asset) => {
      if (id !== RELEASE_ID || ![EXE_NAME, CHECKSUM_NAME].includes(asset?.name)) {
        throw new Error('Only the missing EXE and checksum may be uploaded.');
      }
      const plan = await verifyCurrentAssets(api, expected);
      if (!plan.missing.some((missing) => missing.name === asset.name)) {
        throw new Error('The requested asset is no longer missing.');
      }
      return api.uploadAsset(id, asset);
    },
  };
  const result = await reconcileDraftAssets({
    releaseId: RELEASE_ID, tag: TAG, expectedAssets: expected, api: guardedApi,
  });
  await verifyCurrentAssets(api, expected, true);
  validateRelease(await api.getRelease(RELEASE_ID), RELEASE_ID, TAG);
  await api.verifyTag();
  return result;
}

async function checkSourceAndRelease(api) {
  const prefix = '/repos/' + REPOSITORY;
  const run = await api.json(prefix + '/actions/runs/' + RUN_ID);
  const jobs = await collectPages(async (page) =>
    (await api.json(prefix + '/actions/runs/' + RUN_ID + '/jobs?per_page=100&page=' + page)).jobs);
  const artifact = await api.json(prefix + '/actions/artifacts/' + ARTIFACT_ID);
  verifyProvenance(run, jobs, artifact);
  await api.verifyTag();
  validateRelease(await api.getRelease(RELEASE_ID), RELEASE_ID, TAG);
  const msi = requireMsiIdentity(await listAllAssets(api));
  await verifyAssetContent({ name: MSI_NAME, size: MSI_SIZE, sha256: MSI_SHA256 }, msi, api.getAssetDigest);
}

async function main() {
  if (process.env.GH_REPOSITORY !== REPOSITORY || !process.env.GH_TOKEN) {
    throw new Error('Unexpected repository or missing token.');
  }
  const api = createGithubApi({ repository: REPOSITORY, token: process.env.GH_TOKEN, tag: TAG, commit: BUILD_SHA });
  await checkSourceAndRelease(api);
  if (process.argv[2] === 'preflight') return;
  if (process.argv[2] !== 'upload' || !path.isAbsolute(process.env.RUNNER_TEMP || '')) {
    throw new Error('Invalid attachment phase or runner temp.');
  }
  const originals = await collectExpectedAssets(path.join(process.env.RUNNER_TEMP, 'release-assets'));
  const expected = canonicalAssets(originals);
  await checkSourceAndRelease(api);
  const result = await attachCanonical(api, originals);
  console.log('Verified existing public beta release ID: ' + result.releaseId);
  for (const asset of expected) console.log(asset.sha256 + '  ' + asset.name);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error('Beta canonical attachment failed:', error.message); process.exitCode = 1; });
}
