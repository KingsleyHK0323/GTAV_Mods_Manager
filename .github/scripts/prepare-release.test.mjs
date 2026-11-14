// OpenAI-Codex_ci_fixture_check
import test from 'node:test';
import assert from 'node:assert/strict';
import { validateAssetName, planAssetChanges, validateRelease, collectPages } from './prepare-release.mjs';

const digest = 'a'.repeat(64);
const expected = (name, size = 3) => ({ name, size, sha256: digest });
const actual = (id, name, size = 3, state = 'uploaded') => ({ id, name, state, size });

test('asset names preserve spaces and reject unsafe characters', () => {
  assert.equal(validateAssetName('GTAV Setup 1.0.exe'), 'GTAV Setup 1.0.exe');
  for (const name of ['', 'setup\n.exe', 'setup\r.exe', 'setup*.exe', 'setup?.exe', 'setup[1].exe', 'setup\\x.exe', 'setup/x.exe']) {
    assert.throws(() => validateAssetName(name));
  }
  for (const name of [null, 3, {}, ' setup.exe', 'setup.exe ']) {
    assert.throws(() => validateAssetName(name));
  }
});

test('empty, partial and complete asset sets produce the correct plan', () => {
  const a = expected('setup.exe');
  const b = expected('setup.deb');
  const aa = actual(1, a.name);
  const bb = actual(2, b.name);
  assert.deepEqual(planAssetChanges([], []), { existing: [], missing: [] });
  assert.deepEqual(planAssetChanges([a, b], [aa]), {
    existing: [{ expected: a, actual: aa }], missing: [b],
  });
  assert.deepEqual(planAssetChanges([a, b], [aa, bb]), {
    existing: [{ expected: a, actual: aa }, { expected: b, actual: bb }], missing: [],
  });
});

test('asset planning rejects extra, duplicate, incomplete and malformed records', () => {
  const a = expected('setup.exe');
  assert.throws(() => planAssetChanges([a], [actual(1, 'unexpected.exe')]));
  assert.throws(() => planAssetChanges([a, expected('SETUP.EXE')], []));
  assert.throws(() => planAssetChanges([a], [actual(1, 'setup.exe'), actual(2, 'SETUP.EXE')]));
  assert.throws(() => planAssetChanges([a], [actual(1, a.name, 3, 'new')]));
  assert.throws(() => planAssetChanges([{ ...a, sha256: 'ABC' }], []));
  assert.throws(() => planAssetChanges([{ ...a, size: -1 }], []));
  assert.throws(() => planAssetChanges([a], [{ name: a.name, state: 'uploaded', size: 3 }]));
  assert.throws(() => planAssetChanges(null, []));
  assert.throws(() => planAssetChanges([], null));
});

test('release identity, tag and draft status must match', () => {
  const release = { id: 123, tag_name: 'v1.2.3', draft: true };
  assert.doesNotThrow(() => validateRelease(release, 123, 'v1.2.3'));
  assert.throws(() => validateRelease({ ...release, id: 124 }, 123, 'v1.2.3'));
  assert.throws(() => validateRelease({ ...release, tag_name: 'v1.2.4' }, 123, 'v1.2.3'));
  assert.throws(() => validateRelease({ ...release, draft: false }, 123, 'v1.2.3'));
  assert.throws(() => validateRelease({ ...release, draft: 'true' }, 123, 'v1.2.3'));
  assert.throws(() => validateRelease(null, 123, 'v1.2.3'));
});

test('pagination reads a full page followed by a short page', async () => {
  const calls = [];
  const first = Array.from({ length: 100 }, (_, index) => index);
  const result = await collectPages(async (page) => {
    calls.push(page);
    return page === 1 ? first : [100, 101];
  });
  assert.deepEqual(calls, [1, 2]);
  assert.deepEqual(result, [...first, 100, 101]);
});

test('pagination rejects failed or malformed pages', async () => {
  await assert.rejects(collectPages(async () => { throw new Error('page failed'); }), /page failed/);
  await assert.rejects(collectPages(async () => ({ items: [] })));
  await assert.rejects(collectPages(async (page) => page === 1 ? Array(100).fill(1) : null));
});

test('draft selection requires one unambiguous matching draft', async () => {
  const { selectDraftRelease } = await import('./prepare-release.mjs');
  const draft = { id: 1, tag_name: 'v1.2.3', draft: true };
  assert.equal(selectDraftRelease([], 'v1.2.3'), null);
  assert.deepEqual(selectDraftRelease([draft], 'v1.2.3'), draft);
  assert.throws(() => selectDraftRelease([{ ...draft, draft: false }], 'v1.2.3'));
  assert.throws(() => selectDraftRelease([draft, { ...draft, id: 2 }], 'v1.2.3'));
  assert.throws(() => selectDraftRelease([draft, { id: 1, tag_name: 'v2.0.0', draft: true }], 'v1.2.3'));
});

test('asset digest validation requires the expected size and hash', async () => {
  const { verifyAssetContent } = await import('./prepare-release.mjs');
  const asset = expected('setup.exe');
  const remote = actual(17, asset.name);
  const calls = [];
  await verifyAssetContent(asset, remote, async (...args) => {
    calls.push(args);
    return { size: asset.size, sha256: asset.sha256 };
  });
  assert.deepEqual(calls, [[17, asset.size]]);
  await assert.rejects(verifyAssetContent(asset, remote, async () => ({ size: 4, sha256: digest })));
  await assert.rejects(verifyAssetContent(asset, remote, async () => ({ size: 3, sha256: 'b'.repeat(64) })));
  await assert.rejects(verifyAssetContent(asset, remote, async () => { throw new Error('read failed'); }), /read failed/);
});

function mockReleaseApi(initialAssets, { badDigest = false, publishAfterUpload = false } = {}) {
  const calls = [];
  const assets = [...initialAssets];
  let published = false;
  const api = {
    async getRelease(id) {
      calls.push(['getRelease', id]);
      return { id, tag_name: 'v1.2.3', draft: !published };
    },
    async listAssets(id, page) {
      calls.push(['listAssets', id, page]);
      return page === 1 ? [...assets] : [];
    },
    async getAssetDigest(id, size) {
      calls.push(['getAssetDigest', id, size]);
      return { size, sha256: badDigest ? 'b'.repeat(64) : digest };
    },
    async verifyTag() {
      calls.push(['verifyTag']);
      return true;
    },
    async uploadAsset(id, asset) {
      calls.push(['uploadAsset', id, asset.name]);
      assets.push(actual(assets.length + 1, asset.name, asset.size));
      if (publishAfterUpload) published = true;
    },
  };
  return { api, calls };
}

test('reconciliation refuses unexpected or mismatched assets before upload', async () => {
  const { reconcileDraftAssets } = await import('./prepare-release.mjs');
  const wanted = [expected('setup.exe'), expected('setup.deb')];
  for (const fixture of [
    mockReleaseApi([actual(1, 'extra.exe')]),
    mockReleaseApi([actual(1, 'setup.exe')], { badDigest: true }),
  ]) {
    await assert.rejects(reconcileDraftAssets({ releaseId: 42, tag: 'v1.2.3', expectedAssets: wanted, api: fixture.api }));
    assert.equal(fixture.calls.some(([name]) => name === 'uploadAsset'), false);
  }
});

test('reconciliation uploads only missing assets and rechecks the entire draft', async () => {
  const { reconcileDraftAssets } = await import('./prepare-release.mjs');
  const wanted = [expected('setup.exe'), expected('setup.deb')];
  const { api, calls } = mockReleaseApi([actual(1, 'setup.exe')]);
  await reconcileDraftAssets({ releaseId: 42, tag: 'v1.2.3', expectedAssets: wanted, api });
  assert.deepEqual(calls.filter(([name]) => name === 'uploadAsset'), [['uploadAsset', 42, 'setup.deb']]);
  const uploadIndex = calls.findIndex(([name]) => name === 'uploadAsset');
  assert.ok(calls.slice(0, uploadIndex).some(([name, id]) => name === 'getAssetDigest' && id === 1));
  assert.ok(calls.slice(uploadIndex + 1).some(([name, id]) => name === 'getRelease' && id === 42));
  assert.ok(calls.slice(uploadIndex + 1).some(([name, id, page]) => name === 'listAssets' && id === 42 && page === 1));
  assert.deepEqual(calls.slice(uploadIndex + 1).filter(([name]) => name === 'getAssetDigest').map(([, id]) => id).sort(), [1, 2]);
  assert.ok(calls.some(([name]) => name === 'verifyTag'));
});

test('reconciliation rejects a release published during upload', async () => {
  const { reconcileDraftAssets } = await import('./prepare-release.mjs');
  const { api, calls } = mockReleaseApi([], { publishAfterUpload: true });
  await assert.rejects(reconcileDraftAssets({ releaseId: 42, tag: 'v1.2.3', expectedAssets: [expected('setup.exe')], api }));
  assert.equal(calls.filter(([name]) => name === 'uploadAsset').length, 1);
  assert.ok(calls.filter(([name]) => name === 'getRelease').length >= 2);
});

test('GitHub API dereferences an annotated tag to the expected commit', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  const commit = 'a'.repeat(40);
  const tagObject = 'b'.repeat(40);
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input) => {
    const url = String(input);
    calls.push(url);
    if (url.endsWith('/git/ref/tags/v1.2.3')) {
      return Response.json({ object: { type: 'tag', sha: tagObject } });
    }
    if (url.endsWith(`/git/tags/${tagObject}`)) {
      return Response.json({ object: { type: 'commit', sha: commit } });
    }
    throw new Error(`Unexpected request: ${url}`);
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit });
  await api.verifyTag();
  assert.equal(calls.length, 2);
  assert.ok(calls[0].endsWith('/repos/owner/repo/git/ref/tags/v1.2.3'));
  assert.ok(calls[1].endsWith(`/repos/owner/repo/git/tags/${tagObject}`));
});

test('asset redirect does not forward authorization to the HTTPS target', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  const { createHash } = await import('node:crypto');
  const body = Buffer.from('abc');
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input, options = {}) => {
    calls.push({ url: String(input), headers: new Headers(options.headers), redirect: options.redirect });
    if (calls.length === 1) {
      return new Response(null, { status: 302, headers: { location: 'https://assets.example.test/file' } });
    }
    if (calls.length === 2 && String(input) === 'https://assets.example.test/file') {
      return new Response(body, { status: 200 });
    }
    throw new Error('Unexpected request');
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit: 'a'.repeat(40) });
  assert.deepEqual(await api.getAssetDigest(7, body.length), {
    size: body.length, sha256: createHash('sha256').update(body).digest('hex'),
  });
  assert.equal(calls.length, 2);
  assert.match(calls[0].url, /\/repos\/owner\/repo\/releases\/assets\/7$/);
  assert.equal(calls[0].redirect, 'manual');
  assert.ok(calls[0].headers.has('authorization'));
  assert.equal(calls[1].headers.has('authorization'), false);
});

test('upload uses the exact release ID and encoded filename and rejects a mismatched response', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  const calls = [];
  const body = Buffer.from('abc');
  t.mock.method(globalThis, 'fetch', async (input, options = {}) => {
    const url = new URL(String(input));
    calls.push({ url, options });
    if (url.hostname === 'api.github.com' && url.pathname === '/repos/owner/repo/releases/42') {
      return Response.json({ id: 42, tag_name: 'v1.2.3', draft: true, upload_url: 'https://uploads.github.com/repos/owner/repo/releases/42/assets{?name,label}' });
    }
    if (url.hostname === 'uploads.github.com') {
      return Response.json({ id: 7, name: 'different.exe', size: body.length, state: 'uploaded' }, { status: 201 });
    }
    throw new Error(`Unexpected request: ${url}`);
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit: 'a'.repeat(40) });
  await assert.rejects(api.uploadAsset(42, { name: 'setup file.exe', size: body.length, sha256: digest, data: body }));
  const uploads = calls.filter(({ url }) => url.hostname === 'uploads.github.com');
  assert.equal(uploads.length, 1);
  assert.equal(uploads[0].url.pathname, '/repos/owner/repo/releases/42/assets');
  assert.equal(uploads[0].url.searchParams.get('name'), 'setup file.exe');
  assert.equal(uploads[0].options.method, 'POST');
});

test('GitHub API rejects permission and other error responses', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  t.mock.method(globalThis, 'fetch', async () => Response.json({ message: 'forbidden' }, { status: 403 }));
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit: 'a'.repeat(40) });
  await assert.rejects(api.verifyTag());
  await assert.rejects(api.getAssetDigest(7, 3));
  await assert.rejects(api.uploadAsset(42, { name: 'setup.exe', size: 3, sha256: digest, data: Buffer.from('abc') }));
});

test('upload accepts matching asset metadata and sends the original bytes with authorization', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  const { createHash } = await import('node:crypto');
  const data = Buffer.from('abc');
  const sha256 = createHash('sha256').update(data).digest('hex');
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input, options = {}) => {
    const url = new URL(String(input));
    calls.push({ url, options });
    if (url.hostname === 'api.github.com' && url.pathname === '/repos/owner/repo/releases/42') {
      return Response.json({ id: 42, tag_name: 'v1.2.3', draft: true, upload_url: 'https://uploads.github.com/repos/owner/repo/releases/42/assets{?name,label}' });
    }
    if (url.hostname === 'uploads.github.com') {
      return Response.json({ id: 7, name: 'setup file.exe', size: data.length, state: 'uploaded' }, { status: 201 });
    }
    throw new Error('Unexpected request');
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit: 'a'.repeat(40) });
  await assert.doesNotReject(api.uploadAsset(42, { name: 'setup file.exe', size: data.length, sha256, data }));
  const uploads = calls.filter(({ url }) => url.hostname === 'uploads.github.com');
  assert.equal(uploads.length, 1);
  assert.equal(uploads[0].url.pathname, '/repos/owner/repo/releases/42/assets');
  assert.equal(uploads[0].url.searchParams.get('name'), 'setup file.exe');
  assert.equal(uploads[0].options.method, 'POST');
  assert.deepEqual(uploads[0].options.body, data);
  assert.equal(new Headers(uploads[0].options.headers).get('authorization'), 'Bearer test-token');
});

test('upload rejects a POST permission error after a successful release lookup', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  const { createHash } = await import('node:crypto');
  const data = Buffer.from('abc');
  const sha256 = createHash('sha256').update(data).digest('hex');
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input, options = {}) => {
    const url = new URL(String(input));
    calls.push({ url, options });
    if (url.hostname === 'api.github.com' && url.pathname === '/repos/owner/repo/releases/42') {
      return Response.json({ id: 42, tag_name: 'v1.2.3', draft: true, upload_url: 'https://uploads.github.com/repos/owner/repo/releases/42/assets{?name,label}' });
    }
    if (url.hostname === 'uploads.github.com') return Response.json({ message: 'forbidden' }, { status: 403 });
    throw new Error('Unexpected request');
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit: 'a'.repeat(40) });
  await assert.rejects(api.uploadAsset(42, { name: 'setup.exe', size: data.length, sha256, data }));
  assert.equal(calls.filter(({ url, options }) => url.hostname === 'uploads.github.com' && options.method === 'POST').length, 1);
});

test('draft creation verifies the existing tag and never requests publication', async (t) => {
  const { createGithubApi } = await import('./prepare-release.mjs');
  const commit = 'a'.repeat(40);
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input, options = {}) => {
    const url = new URL(String(input));
    calls.push({ url, options });
    if (url.pathname === '/repos/owner/repo/git/ref/tags/v1.2.3') {
      return Response.json({ object: { type: 'commit', sha: commit } });
    }
    if (url.pathname === '/repos/owner/repo/releases' && options.method === 'POST') {
      return Response.json({ id: 42, tag_name: 'v1.2.3', draft: true, prerelease: false }, { status: 201 });
    }
    throw new Error('Unexpected request');
  });
  const api = createGithubApi({ repository: 'owner/repo', token: 'test-token', tag: 'v1.2.3', commit });
  const release = await api.createDraft();
  assert.equal(release.id, 42);
  assert.equal(release.draft, true);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].url.pathname, '/repos/owner/repo/git/ref/tags/v1.2.3');
  assert.equal(calls[1].options.method, 'POST');
  const payload = JSON.parse(calls[1].options.body);
  assert.equal(payload.tag_name, 'v1.2.3');
  assert.equal(payload.draft, true);
  assert.equal(payload.prerelease, false);
  assert.equal(calls.some(({ options }) => options.method === 'PATCH' || options.method === 'PUT'), false);
});

// OpenAI-Codex_release_impl: Preserve the Windows installer and checksum release gate.
test('Windows-only release requires an installer and its checksum', async () => {
  const { collectExpectedAssets } = await import('./prepare-release.mjs');
  const installer = { name: 'setup.exe', size: 3, sha256: 'a'.repeat(64) };
  const calls = [];
  const assets = await collectExpectedAssets('fixture-root', async (directory, extensions) => {
    calls.push({ directory, extensions });
    return [installer];
  });
  assert.equal(calls.length, 1);
  assert.match(calls[0].directory, /installers-windows$/);
  assert.deepEqual(calls[0].extensions, ['.msi', '.exe']);
  assert.deepEqual(assets.map((asset) => asset.name), ['setup.exe', 'SHA256SUMS-windows.txt']);
  assert.match(assets[1].data.toString('utf8'), /^a{64}  setup\.exe\n$/);
  await assert.rejects(collectExpectedAssets('fixture-root', async () => []), /Windows installer artifact is missing/);
});
