// OpenAI-Codex: Prepare and verify release assets without publishing the draft.
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lstat, readdir, appendFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const PAGE_SIZE = 100;
const MAX_PAGES = 100;
const MAX_JSON_BYTES = 16 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 10 * 60 * 1000;
const SHA256_PATTERN = /^[a-f0-9]{64}$/;
const COMMIT_PATTERN = /^[a-f0-9]{40}$/i;
const TAG_PATTERN = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

export function validateAssetName(name) {
  if (typeof name !== 'string' || name.length > 255 ||
      !/^[A-Za-z0-9](?:[A-Za-z0-9 ._-]*[A-Za-z0-9_-])?$/.test(name)) {
    throw new Error('Invalid or ambiguous release asset filename.');
  }
  return name;
}

function validSize(size) {
  return Number.isSafeInteger(size) && size >= 0;
}

function validId(id) {
  return Number.isSafeInteger(id) && id > 0;
}

export function planAssetChanges(expectedAssets, actualAssets) {
  if (!Array.isArray(expectedAssets) || !Array.isArray(actualAssets)) {
    throw new Error('Asset lists must be arrays.');
  }
  const expectedByName = new Map();
  for (const asset of expectedAssets) {
    if (!asset || typeof asset !== 'object') throw new Error('Invalid expected asset.');
    const name = validateAssetName(asset.name);
    if (!validSize(asset.size) || typeof asset.sha256 !== 'string' || !SHA256_PATTERN.test(asset.sha256)) {
      throw new Error('Invalid expected asset size or digest.');
    }
    const key = name.toLowerCase();
    if (expectedByName.has(key)) throw new Error('Expected asset names are not unique.');
    expectedByName.set(key, asset);
  }

  const actualByName = new Map();
  const ids = new Set();
  for (const asset of actualAssets) {
    if (!asset || typeof asset !== 'object') throw new Error('Invalid existing asset.');
    const name = validateAssetName(asset.name);
    const key = name.toLowerCase();
    if (!validId(asset.id) || ids.has(asset.id) || actualByName.has(key) ||
        asset.state !== 'uploaded' || !validSize(asset.size)) {
      throw new Error('Existing assets have duplicate or invalid metadata.');
    }
    const expected = expectedByName.get(key);
    if (!expected || expected.name !== name || expected.size !== asset.size) {
      throw new Error('An existing asset is unexpected or has a different size or name.');
    }
    ids.add(asset.id);
    actualByName.set(key, asset);
  }

  const existing = [];
  const missing = [];
  for (const expected of expectedAssets) {
    const actual = actualByName.get(expected.name.toLowerCase());
    if (actual) existing.push({ expected, actual });
    else missing.push(expected);
  }
  return { existing, missing };
}

export function validateRelease(release, expectedId, tag) {
  if (!validId(expectedId) || typeof tag !== 'string' || !TAG_PATTERN.test(tag) ||
      !release || release.id !== expectedId || release.tag_name !== tag ||
      release.draft !== true || release.prerelease === true) {
    throw new Error('Release identity, tag or draft state does not match.');
  }
}

export async function collectPages(fetchPage) {
  if (typeof fetchPage !== 'function') throw new Error('A page reader is required.');
  const results = [];
  for (let page = 1; page <= MAX_PAGES; page += 1) {
    const items = await fetchPage(page);
    if (!Array.isArray(items) || items.length > PAGE_SIZE) throw new Error('Invalid paginated API response.');
    results.push(...items);
    if (items.length < PAGE_SIZE) return results;
  }
  throw new Error('The paginated API response exceeded the safety limit.');
}

export function selectDraftRelease(releases, tag) {
  if (!Array.isArray(releases) || typeof tag !== 'string' || !TAG_PATTERN.test(tag)) {
    throw new Error('Invalid release list or tag.');
  }
  const ids = new Set();
  let selected = null;
  for (const release of releases) {
    if (!release || !validId(release.id) || typeof release.tag_name !== 'string' ||
        typeof release.draft !== 'boolean' || ids.has(release.id)) {
      throw new Error('Release listing contains invalid or duplicate records.');
    }
    ids.add(release.id);
    if (release.tag_name === tag) {
      if (selected) throw new Error('More than one release uses the requested tag.');
      validateRelease(release, release.id, tag);
      selected = release;
    }
  }
  return selected;
}

export async function verifyAssetContent(expected, actual, readDigest) {
  const plan = planAssetChanges([expected], [actual]);
  if (plan.existing.length !== 1 || typeof readDigest !== 'function') {
    throw new Error('An existing asset and digest reader are required.');
  }
  const digest = await readDigest(actual.id, expected.size);
  if (!digest || digest.size !== expected.size || digest.sha256 !== expected.sha256) {
    throw new Error('Release asset contents do not match the expected bytes.');
  }
}

export async function reconcileDraftAssets({ releaseId, tag, expectedAssets, api }) {
  if (!api || typeof api.getRelease !== 'function' || typeof api.listAssets !== 'function' ||
      typeof api.getAssetDigest !== 'function' || typeof api.uploadAsset !== 'function' ||
      typeof api.verifyTag !== 'function') {
    throw new Error('The release API is incomplete.');
  }
  validateRelease(await api.getRelease(releaseId), releaseId, tag);
  await api.verifyTag();
  const current = await collectPages((page) => api.listAssets(releaseId, page));
  const plan = planAssetChanges(expectedAssets, current);
  for (const pair of plan.existing) {
    await verifyAssetContent(pair.expected, pair.actual, api.getAssetDigest);
  }
  for (const asset of plan.missing) {
    validateRelease(await api.getRelease(releaseId), releaseId, tag);
    await api.verifyTag();
    await api.uploadAsset(releaseId, asset);
  }

  validateRelease(await api.getRelease(releaseId), releaseId, tag);
  await api.verifyTag();
  const finalAssets = await collectPages((page) => api.listAssets(releaseId, page));
  const finalPlan = planAssetChanges(expectedAssets, finalAssets);
  if (finalPlan.missing.length !== 0) throw new Error('A required release asset is missing.');
  for (const pair of finalPlan.existing) {
    await verifyAssetContent(pair.expected, pair.actual, api.getAssetDigest);
  }
  validateRelease(await api.getRelease(releaseId), releaseId, tag);
  await api.verifyTag();
  return { releaseId, assets: expectedAssets };
}

async function digestStream(stream, maxSize = Number.MAX_SAFE_INTEGER) {
  if (!stream) throw new Error('Asset data is unavailable.');
  const hash = createHash('sha256');
  let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (!Number.isSafeInteger(size) || size > maxSize) throw new Error('Asset exceeds its expected size.');
    hash.update(chunk);
  }
  return { size, sha256: hash.digest('hex') };
}

async function collectInstallerFiles(directory, extensions) {
  const root = await lstat(directory);
  if (!root.isDirectory() || root.isSymbolicLink()) throw new Error('Installer directory is invalid.');
  const files = [];
  async function visit(current) {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name, 'en'));
    for (const entry of entries) {
      const fullPath = path.join(current, entry.name);
      if (entry.isSymbolicLink()) throw new Error('Installer artifacts contain a symbolic link.');
      if (entry.isDirectory()) await visit(fullPath);
      else if (entry.isFile() && extensions.includes(path.extname(entry.name).toLowerCase())) {
        const name = validateAssetName(entry.name);
        const stat = await lstat(fullPath);
        if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0) {
          throw new Error('Installer artifact is invalid or empty.');
        }
        const digest = await digestStream(createReadStream(fullPath), stat.size);
        if (digest.size !== stat.size) throw new Error('Installer size changed during hashing.');
        files.push({ name, path: fullPath, ...digest });
      }
    }
  }
  await visit(directory);
  if (files.length === 0) throw new Error('A platform installer artifact is missing.');
  return files;
}

function checksumAsset(name, installers) {
  const data = Buffer.from(installers.map((asset) => asset.sha256 + '  ' + asset.name + '\n').join(''), 'utf8');
  return { name, size: data.length, sha256: createHash('sha256').update(data).digest('hex'), data };
}

// OpenAI-Codex_release_impl: Collect and verify Windows-only release assets.
export async function collectExpectedAssets(root, collect = collectInstallerFiles) {
  const windows = await collect(path.join(root, 'installers-windows'), ['.msi', '.exe']);
  if (!Array.isArray(windows) || windows.length === 0) throw new Error('A Windows installer artifact is missing.');
  const assets = [...windows, checksumAsset('SHA256SUMS-windows.txt', windows)];
  planAssetChanges(assets, []);
  return assets;
}

function createGithubApi({ repository, token, tag, commit }) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository) || !token ||
      !TAG_PATTERN.test(tag) || !COMMIT_PATTERN.test(commit)) {
    throw new Error('Required GitHub release context is invalid.');
  }
  const repoPath = '/repos/' + repository;
  const apiBase = 'https://api.github.com';
  const auth = { Authorization: 'Bearer ' + token, 'X-GitHub-Api-Version': '2022-11-28' };

  async function request(url, options = {}) {
    try {
      return await fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
    } catch {
      throw new Error('GitHub request failed or timed out.');
    }
  }

  async function json(endpoint, options = {}) {
    const response = await request(apiBase + endpoint, {
      method: options.method || 'GET',
      headers: { ...auth, Accept: 'application/vnd.github+json',
        ...(options.body ? { 'Content-Type': 'application/json' } : {}) },
      ...(options.body ? { body: JSON.stringify(options.body) } : {}),
    });
    if (!response.ok) throw new Error('GitHub API returned HTTP ' + response.status + '.');
    const parts = [];
    let size = 0;
    if (!response.body) throw new Error('GitHub API returned an empty response.');
    for await (const part of response.body) {
      size += part.length;
      if (size > MAX_JSON_BYTES) throw new Error('GitHub API response exceeded the safety limit.');
      parts.push(Buffer.from(part));
    }
    try { return JSON.parse(Buffer.concat(parts).toString('utf8')); }
    catch { throw new Error('GitHub API returned invalid JSON.'); }
  }

  async function verifyTag() {
    let object = (await json(repoPath + '/git/ref/tags/' + encodeURIComponent(tag))).object;
    const seen = new Set();
    for (let depth = 0; depth < 8; depth += 1) {
      if (!object || !COMMIT_PATTERN.test(object.sha) || seen.has(object.sha)) {
        throw new Error('Release tag points to an invalid Git object.');
      }
      if (object.type === 'commit') {
        if (object.sha.toLowerCase() !== commit.toLowerCase()) throw new Error('Release tag no longer points to the build commit.');
        return true;
      }
      if (object.type !== 'tag') throw new Error('Release tag does not point to a commit.');
      seen.add(object.sha);
      object = (await json(repoPath + '/git/tags/' + object.sha)).object;
    }
    throw new Error('Release tag contains too many nested annotated tags.');
  }

  async function getRelease(id) { return json(repoPath + '/releases/' + id); }
  async function listReleases(page) { return json(repoPath + '/releases?per_page=100&page=' + page); }
  async function listAssets(id, page) { return json(repoPath + '/releases/' + id + '/assets?per_page=100&page=' + page); }

  async function getAssetDigest(id, expectedSize) {
    if (!validId(id) || !validSize(expectedSize)) throw new Error('Invalid asset lookup.');
    let response;
    try {
      response = await fetch(apiBase + repoPath + '/releases/assets/' + id, {
        headers: { ...auth, Accept: 'application/octet-stream' }, redirect: 'manual',
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch { throw new Error('GitHub asset download failed.'); }
    if (response.status === 302) {
      let destination;
      try { destination = new URL(response.headers.get('location')); }
      catch { throw new Error('GitHub asset redirect is invalid.'); }
      if (destination.protocol !== 'https:' || destination.username || destination.password || destination.hash) {
        throw new Error('GitHub asset redirect is unsafe.');
      }
      response = await request(destination, { headers: { Accept: 'application/octet-stream' } });
    }
    if (response.status !== 200) throw new Error('GitHub asset download returned HTTP ' + response.status + '.');
    return digestStream(response.body, expectedSize);
  }

  async function uploadAsset(id, asset) {
    const release = await getRelease(id);
    validateRelease(release, id, tag);
    if (typeof release.upload_url !== 'string' || !release.upload_url.endsWith('{?name,label}')) {
      throw new Error('Release upload URL is invalid.');
    }
    let target;
    try { target = new URL(release.upload_url.slice(0, -'{?name,label}'.length)); }
    catch { throw new Error('Release upload URL is invalid.'); }
    const expectedPath = repoPath + '/releases/' + id + '/assets';
    if (target.protocol !== 'https:' || target.hostname !== 'uploads.github.com' ||
        target.port || target.username || target.password || target.pathname.toLowerCase() !== expectedPath.toLowerCase() ||
        target.search || target.hash) throw new Error('Release upload URL is unsafe.');
    target.searchParams.set('name', validateAssetName(asset.name));
    const body = asset.data ?? createReadStream(asset.path);
    const response = await request(target, {
      method: 'POST', headers: { ...auth, Accept: 'application/vnd.github+json',
        'Content-Type': 'application/octet-stream', 'Content-Length': String(asset.size) },
      body, ...(asset.data ? {} : { duplex: 'half' }),
    });
    if (response.status !== 201) throw new Error('GitHub asset upload returned HTTP ' + response.status + '.');
    const result = await response.json();
    planAssetChanges([asset], [result]);
  }

  async function createDraft() {
    await verifyTag();
    const release = await json(repoPath + '/releases', { method: 'POST', body: {
      tag_name: tag, name: 'GTAV Mods Manager ' + tag, draft: true, prerelease: false,
      generate_release_notes: true,
      body: 'Windows installers are unsigned and may show a SmartScreen warning on first run.',
    } });
    validateRelease(release, release.id, tag);
    return release;
  }

  return { json, verifyTag, getRelease, listReleases, listAssets, getAssetDigest, uploadAsset, createDraft };
}

async function main() {
  const repository = process.env.GH_REPOSITORY || '';
  const token = process.env.GH_TOKEN || '';
  const tag = process.env.RELEASE_TAG || '';
  const commit = process.env.GITHUB_SHA || '';
  const runnerTemp = process.env.RUNNER_TEMP || '';
  if (!path.isAbsolute(runnerTemp)) throw new Error('Runner temporary directory is unavailable.');
  const expectedAssets = await collectExpectedAssets(path.join(runnerTemp, 'release-assets'));
  const api = createGithubApi({ repository, token, tag, commit });
  const repositoryInfo = await api.json('/repos/' + repository);
  if (repositoryInfo?.permissions?.push !== true) throw new Error('GitHub did not confirm release write access.');
  await api.verifyTag();
  let releases = await collectPages(api.listReleases);
  let release = selectDraftRelease(releases, tag);
  if (!release) {
    release = await api.createDraft();
    releases = await collectPages(api.listReleases);
    const selected = selectDraftRelease(releases, tag);
    if (!selected || selected.id !== release.id) throw new Error('New draft is missing or ambiguous.');
  }
  const result = await reconcileDraftAssets({ releaseId: release.id, tag, expectedAssets, api });
  const selected = selectDraftRelease(await collectPages(api.listReleases), tag);
  if (!selected || selected.id !== result.releaseId) throw new Error('Draft release identity changed.');
  const lines = [
    '## Verified release draft', '',
    'Tag: ' + tag, 'Commit: ' + commit, 'Release ID: ' + result.releaseId,
    'Run: ' + (process.env.GITHUB_RUN_ID || 'unknown') + ', attempt: ' + (process.env.GITHUB_RUN_ATTEMPT || 'unknown'), '',
    '| Asset | Bytes | SHA-256 |', '| --- | ---: | --- |',
    ...result.assets.map((asset) => '| ' + asset.name + ' | ' + asset.size + ' | ' + asset.sha256 + ' |'),
    '', 'The draft remains unpublished. Recheck its assets before manual publication.', '',
  ];
  const summary = lines.join('\n');
  if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, summary, 'utf8');
  console.log(summary);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error('Release draft preparation failed:', error.message); process.exitCode = 1; });
}

export { createGithubApi };
