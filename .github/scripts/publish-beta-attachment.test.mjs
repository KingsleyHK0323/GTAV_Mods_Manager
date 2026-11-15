// OpenAI-Codex_release_impl: Test exact release binding and non-overwriting attachment.
import test from 'node:test';
import assert from 'node:assert/strict';
import { checkRelease, RELEASE_ID } from './publish-beta-attachment.mjs';
import { TAG } from './publish-beta-recovery.mjs';
import { reconcileDraftAssets } from './prepare-beta-release.mjs';

const release = { id: RELEASE_ID, tag_name: TAG, draft: false, prerelease: false };
test('attachment accepts only the exact public non-prerelease beta', () => {
  assert.doesNotThrow(() => checkRelease(release));
  assert.throws(() => checkRelease({ ...release, id: RELEASE_ID + 1 }));
  assert.throws(() => checkRelease({ ...release, tag_name: 'v1.0.0' }));
  assert.throws(() => checkRelease({ ...release, draft: true }));
  assert.throws(() => checkRelease({ ...release, prerelease: true }));
});

function fixture(existingDigest) {
  const expected = { name: 'setup.exe', size: 3, sha256: 'a'.repeat(64) };
  const asset = { id: 1, name: 'setup.exe', size: 3, state: 'uploaded' };
  const writes = [];
  const api = {
    getRelease: async () => release,
    verifyTag: async () => true,
    listAssets: async () => [asset],
    getAssetDigest: async () => ({ size: 3, sha256: existingDigest }),
    uploadAsset: async (_, value) => writes.push(value),
  };
  return { expected, writes, api };
}
test('existing matching assets are verified and never overwritten', async () => {
  const f = fixture('a'.repeat(64));
  await reconcileDraftAssets({ releaseId: RELEASE_ID, tag: TAG, expectedAssets: [f.expected], api: f.api });
  assert.deepEqual(f.writes, []);
});
test('existing mismatched assets stop the process before any upload', async () => {
  const f = fixture('b'.repeat(64));
  await assert.rejects(reconcileDraftAssets({ releaseId: RELEASE_ID, tag: TAG, expectedAssets: [f.expected], api: f.api }));
  assert.deepEqual(f.writes, []);
});

// OpenAI-Codex: Cover uploads into an empty release and subsequent content verification.
function emptyReleaseFixture(corruptReadback = false) {
  const expected = [
    { name: 'setup.exe', size: 3, sha256: 'a'.repeat(64) },
    { name: 'SHA256SUMS-windows.txt', size: 5, sha256: 'b'.repeat(64) },
  ];
  const existing = [];
  const uploads = [];
  const reads = [];
  const api = {
    getRelease: async () => release,
    verifyTag: async () => true,
    listAssets: async () => existing.map((asset) => ({ ...asset })),
    getAssetDigest: async (id) => {
      reads.push(id);
      const asset = expected[id - 1];
      return { size: asset.size, sha256: corruptReadback ? 'f'.repeat(64) : asset.sha256 };
    },
    uploadAsset: async (releaseId, asset) => {
      assert.equal(releaseId, RELEASE_ID);
      uploads.push(asset.name);
      existing.push({ id: uploads.length, name: asset.name, size: asset.size, state: 'uploaded' });
    },
  };
  return { expected, uploads, reads, api };
}

test('missing assets are uploaded and then read back for verification', async () => {
  const f = emptyReleaseFixture();
  const result = await reconcileDraftAssets({ releaseId: RELEASE_ID, tag: TAG, expectedAssets: f.expected, api: f.api });
  assert.equal(result.releaseId, RELEASE_ID);
  assert.deepEqual(f.uploads, f.expected.map((asset) => asset.name));
  assert.deepEqual(f.reads, [1, 2]);
});

test('an uploaded asset with a mismatched read-back digest fails verification', async () => {
  const f = emptyReleaseFixture(true);
  await assert.rejects(
    reconcileDraftAssets({ releaseId: RELEASE_ID, tag: TAG, expectedAssets: f.expected, api: f.api }),
    /Release asset contents do not match/,
  );
  assert.deepEqual(f.uploads, f.expected.map((asset) => asset.name));
  assert.deepEqual(f.reads, [1]);
});
