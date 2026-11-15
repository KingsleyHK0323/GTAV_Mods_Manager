// OpenAI-Codex_canonical_attachment_impl: Test safe attachment using canonical public asset names.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { canonicalAssets, attachCanonical } from './publish-beta-canonical.mjs';

const releaseId = 445702174;
const msiId = 749380433;
const msiHash = '903da42569003df651c54b207a4985c9ebaacf68172a1fe2a917ab820ffebd8d';
const exeHash = '33e6650c646c4f5f663bc24eea4f1dbd127174ad94b07f94b4d02dbe9a9fc930';
const publicMsi = 'GTAV.Mods.Manager_1.0.0_x64_en-US.msi';
const publicExe = 'GTAV.Mods.Manager_1.0.0_x64-setup.exe';
const checksumName = 'SHA256SUMS-windows.txt';

function originalAssets() {
  const msi = {
    name: 'GTAV Mods Manager_1.0.0_x64_en-US.msi',
    path: '/synthetic/original.msi', size: 8593408, sha256: msiHash,
  };
  const exe = {
    name: 'GTAV Mods Manager_1.0.0_x64-setup.exe',
    path: '/synthetic/original.exe', size: 6397736, sha256: exeHash,
  };
  const data = Buffer.from(
    msiHash + '  GTAV Mods Manager_1.0.0_x64_en-US.msi\n' +
    exeHash + '  GTAV Mods Manager_1.0.0_x64-setup.exe\n',
    'utf8',
  );
  return [msi, exe, {
    name: checksumName, data, size: data.length,
    sha256: createHash('sha256').update(data).digest('hex'),
  }];
}

function fixture({ wrongMsiId = false, badMsi = false, extra = false,
  removeMsiAfterFirstList = false, badExe = false } = {}) {
  const assets = [{
    id: wrongMsiId ? 1 : msiId,
    name: publicMsi, size: 8593408, state: 'uploaded',
  }];
  if (extra) assets.push({ id: 42, name: 'unexpected.exe', size: 1, state: 'uploaded' });
  const uploaded = [];
  const reads = [];
  const events = [];
  const payloads = new Map();
  let lists = 0;
  const api = {
    getRelease: async (id) => {
      assert.equal(id, releaseId);
      return { id: releaseId, tag_name: 'v1.0.0-beta', draft: false, prerelease: false };
    },
    verifyTag: async () => true,
    listAssets: async (id, page) => {
      assert.equal(id, releaseId);
      if (page !== 1) return [];
      lists += 1;
      if (removeMsiAfterFirstList && lists > 1) return assets.filter((a) => a.name !== publicMsi);
      return assets.map((asset) => ({ ...asset }));
    },
    getAssetDigest: async (id, size) => {
      reads.push(id);
      events.push('read:' + id);
      if (id === msiId || (wrongMsiId && id === 1)) {
        return { size, sha256: badMsi ? '0'.repeat(64) : msiHash };
      }
      const payload = payloads.get(id);
      if (!payload) throw new Error('Unknown uploaded asset');
      return { size, sha256: badExe && payload.name === publicExe ? 'f'.repeat(64) : payload.sha256 };
    },
    uploadAsset: async (id, asset) => {
      assert.equal(id, releaseId);
      uploaded.push(asset.name);
      events.push('upload:' + asset.name);
      const assetId = 800000000 + uploaded.length;
      payloads.set(assetId, asset);
      assets.push({ id: assetId, name: asset.name, size: asset.size, state: 'uploaded' });
    },
  };
  return { api, assets, uploaded, reads, events, payloads };
}

test('checksum uses the exact public installer names while retaining original paths', () => {
  const assets = canonicalAssets(originalAssets());
  assert.deepEqual(assets.map((a) => a.name), [publicMsi, publicExe, checksumName]);
  assert.equal(assets[0].path, '/synthetic/original.msi');
  assert.equal(assets[1].path, '/synthetic/original.exe');
  assert.equal(assets[2].data.toString('utf8'),
    msiHash + '  GTAV.Mods.Manager_1.0.0_x64_en-US.msi\n' +
    exeHash + '  GTAV.Mods.Manager_1.0.0_x64-setup.exe\n');
});

test('verifies and preserves existing MSI, uploading only the missing EXE and checksum', async () => {
  const f = fixture();
  await attachCanonical(f.api, originalAssets());
  assert.deepEqual(f.uploaded, [publicExe, checksumName]);
  assert.equal(f.assets.find((a) => a.name === publicMsi).id, msiId);
  assert.ok(f.reads.filter((id) => id === msiId).length >= 2);
  assert.ok(f.events.indexOf('read:' + msiId) < f.events.indexOf('upload:' + publicExe));
  const checksum = [...f.payloads.values()].find((a) => a.name === checksumName);
  assert.equal(checksum.data.toString('utf8'),
    msiHash + '  GTAV.Mods.Manager_1.0.0_x64_en-US.msi\n' +
    exeHash + '  GTAV.Mods.Manager_1.0.0_x64-setup.exe\n');
});

test('refuses an unexpected MSI ID before any upload', async () => {
  const f = fixture({ wrongMsiId: true });
  await assert.rejects(attachCanonical(f.api, originalAssets()));
  assert.deepEqual(f.uploaded, []);
});

test('refuses a mismatched existing MSI digest before any upload', async () => {
  const f = fixture({ badMsi: true });
  await assert.rejects(attachCanonical(f.api, originalAssets()));
  assert.deepEqual(f.uploaded, []);
});

test('refuses an unexpected existing asset before any upload', async () => {
  const f = fixture({ extra: true });
  await assert.rejects(attachCanonical(f.api, originalAssets()));
  assert.deepEqual(f.uploaded, []);
});

test('does not replace a missing MSI when it disappears after the first check', async () => {
  const f = fixture({ removeMsiAfterFirstList: true });
  await assert.rejects(attachCanonical(f.api, originalAssets()));
  assert.deepEqual(f.uploaded, []);
});

test('stops before uploading the checksum when EXE readback has the wrong digest', async () => {
  const f = fixture({ badExe: true });
  await assert.rejects(attachCanonical(f.api, originalAssets()));
  assert.deepEqual(f.uploaded, [publicExe]);
});

test('rejects an original EXE with the wrong reviewed byte size', () => {
  const originals = originalAssets();
  originals[1] = { ...originals[1], size: 6397735 };
  assert.throws(() => canonicalAssets(originals));
});

test('rejects a changed MSI ID in the final asset list', async () => {
  const f = fixture();
  const originalList = f.api.listAssets;
  f.api.listAssets = async (id, page) => {
    const assets = await originalList(id, page);
    if (f.uploaded.length === 2) {
      return assets.map((asset) => asset.name === publicMsi ? { ...asset, id: 1 } : asset);
    }
    return assets;
  };
  await assert.rejects(attachCanonical(f.api, originalAssets()));
  assert.deepEqual(f.uploaded, [publicExe, checksumName]);
});
