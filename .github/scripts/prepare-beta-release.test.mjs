// OpenAI-Codex_release_impl: Verify direct public beta creation and release state.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createGithubApi, validateRelease } from './prepare-beta-release.mjs';
import { planAssetChanges } from './prepare-beta-release.mjs';
import { verifyAssetContent } from './prepare-beta-release.mjs';
import { assertNoExistingRelease } from './prepare-beta-release.mjs';

const tag = 'v1.0.0-beta';
const commit = 'a'.repeat(40);
test('only the requested tag and public non-prerelease state are accepted', () => {
  const release = { id: 42, tag_name: tag, draft: false, prerelease: false };
  assert.doesNotThrow(() => validateRelease(release, 42, tag));
  assert.throws(() => validateRelease({ ...release, draft: true }, 42, tag));
  assert.throws(() => validateRelease({ ...release, prerelease: true }, 42, tag));
  assert.throws(() => validateRelease({ ...release, prerelease: undefined }, 42, tag));
  assert.throws(() => validateRelease({ ...release, tag_name: 'v1.0.0' }, 42, 'v1.0.0'));
});
test('creation verifies the tag and directly requests a public non-prerelease', async (t) => {
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input, options = {}) => {
    const url = new URL(String(input));
    calls.push({ url, options });
    if (url.pathname === '/repos/owner/repo/git/ref/tags/' + tag) {
      return Response.json({ object: { type: 'commit', sha: commit } });
    }
    if (url.pathname === '/repos/owner/repo/releases' && options.method === 'POST') {
      return Response.json({ id: 42, tag_name: tag, draft: false, prerelease: false }, { status: 201 });
    }
    throw new Error('Unexpected request');
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag, commit });
  await api.createDraft();
  assert.equal(calls.length, 2);
  const payload = JSON.parse(calls[1].options.body);
  assert.equal(payload.tag_name, tag);
  assert.equal(payload.target_commitish, commit);
  assert.equal(payload.draft, false);
  assert.equal(payload.prerelease, false);
  assert.equal(calls.some(({ options }) => options.method === 'PATCH' || options.method === 'PUT'), false);
});

test('asset reconciliation rejects mismatched existing files', () => {
  const expected = [{ name: 'setup.exe', size: 4, sha256: 'a'.repeat(64) }];
  const valid = { id: 1, name: 'setup.exe', size: 4, state: 'uploaded' };
  assert.equal(planAssetChanges(expected, [valid]).existing.length, 1);
  assert.throws(() => planAssetChanges(expected, [{ ...valid, size: 5 }]));
  assert.throws(() => planAssetChanges(expected, [{ ...valid, name: 'unknown.exe' }]));
  assert.throws(() => planAssetChanges(expected, [valid, valid]));
});

test('asset verification rejects a digest that differs from the built installer', async () => {
  const expected = { name: 'setup.exe', size: 4, sha256: 'a'.repeat(64) };
  const actual = { id: 1, name: 'setup.exe', size: 4, state: 'uploaded' };
  await verifyAssetContent(expected, actual, async () => ({ size: 4, sha256: 'a'.repeat(64) }));
  await assert.rejects(verifyAssetContent(expected, actual, async () => ({ size: 4, sha256: 'b'.repeat(64) })));
  await assert.rejects(verifyAssetContent(expected, actual, async () => ({ size: 3, sha256: 'a'.repeat(64) })));
});

test('existing beta release is rejected before any publication or asset upload', async () => {
  const existing = { id: 42, tag_name: tag, draft: false, prerelease: false };
  let writes = 0;
  async function publishIfAbsent(releases) {
    assertNoExistingRelease(releases, tag);
    writes += 1;
  }
  await assert.rejects(publishIfAbsent([existing]), /already exists/);
  assert.equal(writes, 0);
  await publishIfAbsent([]);
  assert.equal(writes, 1);
});
