// OpenAI-Codex_test_plan: Test only isolated predicates in the inactive candidate.
// These checks do not prove the complete test suite passes or auto-merge is safe.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { TextDecoder } from 'node:util';
import { createContext, runInContext } from 'node:vm';

const candidatePath = new URL('../candidates/dependabot-auto-merge.candidate.yml', import.meta.url);
const candidate = readFileSync(candidatePath, 'utf8');
const heredocs = [...candidate.matchAll(/^[ \t]*node <<'NODE'\r?\n([\s\S]*?)^[ \t]*NODE[ \t]*\r?$/gm)];
assert.equal(heredocs.length, 1, 'expected one candidate Node heredoc');
assert.match(candidate, /false && \(/, 'the candidate job must remain disabled');

const entryPoint = 'main().catch((error) => {';
const source = heredocs[0][1];
assert.equal(source.split(entryPoint).length, 2, 'expected one main entry point');
const declarations = source.slice(0, source.indexOf(entryPoint));
const repository = 'example/repository';
const repositoryId = 12345;
const head = 'a'.repeat(40);
const base = 'b'.repeat(40);

// The candidate only receives synthetic environment, filesystem and network objects.
// The candidate main function and workflow are never invoked by these tests.
const context = createContext({
  process: { env: {
    GITHUB_API_URL: 'https://api.example.invalid',
    GITHUB_REPOSITORY: repository,
    GITHUB_REPOSITORY_ID: String(repositoryId),
    GH_TOKEN: 'synthetic-token',
  } },
  URL,
  URLSearchParams,
  Buffer,
  TextDecoder,
  require(name) {
    if (name !== 'node:fs') throw new Error('Unexpected module: ' + name);
    return Object.freeze({
      readFileSync() { throw new Error('Unexpected filesystem read'); },
      writeFileSync() { throw new Error('Unexpected filesystem write'); },
    });
  },
  fetch() { throw new Error('Unexpected network request'); },
  console: Object.freeze({ log() {}, error() {} }),
});

const policy = runInContext(declarations + `
  ;({ policyVersion, policyUniqueJson, policyRegistryUrl, policySameIdentity,
      policyTrustedDependabotSigner, policyOneDependency, policyLockMatches })
`, context, { filename: 'inactive-dependabot-policy.js', timeout: 5000 });

function identity() {
  return {
    number: 42,
    state: 'open',
    draft: false,
    auto_merge: null,
    user: { login: 'dependabot[bot]', id: 49699333, type: 'Bot' },
    base: { ref: 'Source_Code', sha: base, repo: { id: repositoryId, full_name: repository } },
    head: { sha: head, repo: { id: repositoryId, full_name: repository } },
  };
}

function manifests() {
  return [
    { name: 'fixture', dependencies: { sample: '^1.2.3' }, scripts: { build: 'build' } },
    { name: 'fixture', dependencies: { sample: '^1.2.4' }, scripts: { build: 'build' } },
  ];
}

function locks() {
  const oldIntegrity = 'sha512-' + Buffer.alloc(64, 1).toString('base64');
  const newIntegrity = 'sha512-' + Buffer.alloc(64, 2).toString('base64');
  return [
    { lockfileVersion: 3, packages: {
      '': { dependencies: { sample: '^1.2.3' } },
      'node_modules/sample': {
        version: '1.2.3', resolved: 'https://registry.npmjs.org/sample/-/sample-1.2.3.tgz',
        integrity: oldIntegrity,
      },
    } },
    { lockfileVersion: 3, packages: {
      '': { dependencies: { sample: '^1.2.4' } },
      'node_modules/sample': {
        version: '1.2.4', resolved: 'https://registry.npmjs.org/sample/-/sample-1.2.4.tgz',
        integrity: newIntegrity,
      },
    } },
  ];
}

test('version parsing accepts only strict complete stable versions', () => {
  assert.equal(policy.policyVersion('^1.2.4').bare, '1.2.4');
  assert.equal(policy.policyVersion('~3.4.5').prefix, '~');
  for (const spec of ['^1.2.4\n', '^0.2.3', '^1.2', '*', 'latest',
    '^1.2.4-beta.1', '^1.2.9007199254740992']) {
    assert.equal(policy.policyVersion(spec), null, spec);
  }
});

test('JSON parsing rejects duplicate keys and non-canonical or unsafe numbers', () => {
  assert.equal(policy.policyUniqueJson('{"n":3,"nested":{"value":true}}').n, 3);
  for (const text of ['{"a":1,"a":2}', '{"a":1,"\\u0061":2}', '{"n":-0}',
    '{"n":1e2}', '{"n":3.0000000000000000000001}', '{"n":9007199254740992}']) {
    assert.throws(() => policy.policyUniqueJson(text), undefined, text);
  }
});

test('registry URL must equal the exact expected HTTPS tarball URL', () => {
  const valid = 'https://registry.npmjs.org/sample/-/sample-1.2.4.tgz';
  assert.equal(policy.policyRegistryUrl(valid, 'sample', '1.2.4'), true);
  for (const invalid of [valid + '\n', valid + '?', valid + '#', valid.replace('https:', 'http:'),
    valid.replace('registry.npmjs.org', 'other.example')]) {
    assert.equal(policy.policyRegistryUrl(invalid, 'sample', '1.2.4'), false, invalid);
  }
});

test('identity rejects absent, armed, mismatched or stale PR fields', () => {
  const valid = identity();
  assert.equal(policy.policySameIdentity(valid, 42, head, base), true);
  for (const mutate of [
    (pr) => { delete pr.draft; },
    (pr) => { delete pr.auto_merge; },
    (pr) => { pr.auto_merge = {}; },
    (pr) => { pr.user.id = 1; },
    (pr) => { pr.head.repo.id = 999; },
    (pr) => { pr.base.ref = 'Other'; },
    (pr) => { pr.head.sha = 'c'.repeat(40); },
  ]) {
    const pr = identity();
    mutate(pr);
    assert.equal(policy.policySameIdentity(pr, 42, head, base), false);
  }
});

test('unknown signer identity cannot authorize a mutation', () => {
  assert.equal(policy.policyTrustedDependabotSigner(), false);
});

test('manifest comparison accepts only one direct patch change', () => {
  const [before, after] = manifests();
  const change = policy.policyOneDependency(before, after);
  assert.equal(change.name, 'sample');
  assert.equal(change.oldVersion.bare, '1.2.3');
  assert.equal(change.newVersion.bare, '1.2.4');
  const major = JSON.parse(JSON.stringify(after));
  major.dependencies.sample = '^2.0.0';
  assert.equal(policy.policyOneDependency(before, major), null);
  const changedScript = JSON.parse(JSON.stringify(after));
  changedScript.scripts.build = 'unexpected';
  assert.equal(policy.policyOneDependency(before, changedScript), null);
  const groupedBefore = JSON.parse(JSON.stringify(before));
  const groupedAfter = JSON.parse(JSON.stringify(after));
  groupedBefore.dependencies.second = '^3.4.5';
  groupedAfter.dependencies.second = '^3.4.6';
  assert.equal(policy.policyOneDependency(groupedBefore, groupedAfter), null);
});

test('lock comparison rejects transitive changes and invalid integrity', () => {
  const [beforeManifest, afterManifest] = manifests();
  const change = policy.policyOneDependency(beforeManifest, afterManifest);
  const [before, after] = locks();
  assert.equal(policy.policyLockMatches(before, after, change), true);
  const transitive = JSON.parse(JSON.stringify(after));
  transitive.packages['node_modules/sample'].dependencies = { other: '^2.0.0' };
  assert.equal(policy.policyLockMatches(before, transitive, change), false);
  const invalidIntegrity = JSON.parse(JSON.stringify(after));
  invalidIntegrity.packages['node_modules/sample'].integrity = 'sha512-invalid';
  assert.equal(policy.policyLockMatches(before, invalidIntegrity, change), false);
});

// OpenAI-Codex_test_plan: Exercise the candidate dispatcher with explicit synthetic APIs.
// No entry point, workflow, real network, filesystem mutation, or merge is invoked.
async function simulateProcess({ armed }) {
  const [oldManifest, newManifest] = manifests();
  const [oldLock, newLock] = locks();
  const manifestPath = 'crates/gtavmm-app/package.json';
  const lockPath = 'crates/gtavmm-app/package-lock.json';
  const baseTree = 'c'.repeat(40);
  const headTree = 'd'.repeat(40);
  const root = '/repos/' + repository;
  const calls = [];
  const messages = [];
  const pr = { ...identity(), changed_files: 2, commits: 1, auto_merge: armed ? {} : null };

  function blob(value) {
    const bytes = Buffer.from(JSON.stringify(value), 'utf8');
    const sha = createHash('sha1').update('blob ' + bytes.length + '\0').update(bytes).digest('hex');
    return { type: 'file', encoding: 'base64', size: bytes.length, sha, content: bytes.toString('base64') };
  }

  const contents = {
    [base]: { [manifestPath]: blob(oldManifest), [lockPath]: blob(oldLock) },
    [head]: { [manifestPath]: blob(newManifest), [lockPath]: blob(newLock) },
  };

  function tree(sha, ref) {
    return {
      sha, truncated: false,
      tree: [manifestPath, lockPath].map((path) => ({
        path, mode: '100644', type: 'blob', sha: contents[ref][path].sha,
      })),
    };
  }

  function response(value) {
    return { ok: true, status: 200, text: async () => JSON.stringify(value) };
  }

  const fakeFetch = async (input, init = {}) => {
    const url = new URL(input);
    const method = init.method || 'GET';
    const path = url.pathname;
    calls.push({ method, path, query: url.search });
    if (method !== 'GET' || url.origin !== 'https://api.example.invalid' ||
        url.username || url.password || url.hash ||
        init.headers?.Authorization !== 'Bearer synthetic-token') {
      throw new Error('Unexpected synthetic request');
    }
    if (path === root + '/pulls/42' && !url.search) return response(pr);
    if (path === root + '/branches/Source_Code' && !url.search) {
      return response({ name: 'Source_Code', commit: { sha: base } });
    }
    if (path === root + '/commits/' + head && !url.search) {
      return response({
        sha: head,
        author: { login: 'dependabot[bot]', id: 49699333, type: 'Bot' },
        committer: { login: 'dependabot[bot]', id: 49699333, type: 'Bot' },
        commit: { tree: { sha: headTree }, verification: { verified: true, reason: 'valid' } },
        parents: [{ sha: base }],
      });
    }
    if (path === root + '/pulls/42/files' && url.search === '?per_page=100&page=1') {
      return response([manifestPath, lockPath].map((filename) => ({
        filename, status: 'modified', sha: contents[head][filename].sha,
      })));
    }
    if (path === root + '/git/commits/' + base && !url.search) {
      return response({ sha: base, tree: { sha: baseTree } });
    }
    if (path === root + '/git/trees/' + baseTree && url.search === '?recursive=1') {
      return response(tree(baseTree, base));
    }
    if (path === root + '/git/trees/' + headTree && url.search === '?recursive=1') {
      return response(tree(headTree, head));
    }
    for (const ref of [base, head]) {
      for (const filename of [manifestPath, lockPath]) {
        if (path === root + '/contents/' + filename && url.search === '?ref=' + ref) {
          return response(contents[ref][filename]);
        }
      }
    }
    if (path === root + '/actions/workflows/ci.yml' && !url.search) {
      return response({ id: 123, name: 'CI', path: '.github/workflows/ci.yml' });
    }
    const expectedQuery = '?head_sha=' + head + '&event=pull_request&per_page=100&page=1';
    if (path === root + '/actions/workflows/123/runs' && url.search === expectedQuery) {
      const repo = { id: repositoryId, full_name: repository };
      return response({ workflow_runs: [{
        workflow_id: 123, event: 'pull_request', head_sha: head,
        repository: repo, head_repository: repo, run_number: 1, run_attempt: 1,
        status: 'completed', conclusion: 'success',
        pull_requests: [{ number: 42, head: { sha: head, repo },
          base: { sha: base, ref: 'Source_Code', repo } }],
      }] });
    }
    throw new Error('Unexpected synthetic endpoint: ' + path + url.search);
  };

  const sandbox = createContext({
    process: { env: {
      GITHUB_API_URL: 'https://api.example.invalid', GITHUB_REPOSITORY: repository,
      GITHUB_REPOSITORY_ID: String(repositoryId), GH_TOKEN: 'synthetic-token',
    } },
    URL, URLSearchParams, Buffer, TextDecoder,
    require(name) {
      if (name === 'node:fs') return Object.freeze({
        readFileSync() { throw new Error('Unexpected filesystem read'); },
        writeFileSync() { throw new Error('Unexpected filesystem write'); },
      });
      if (name === 'node:crypto') return Object.freeze({ createHash });
      throw new Error('Unexpected module: ' + name);
    },
    fetch: fakeFetch,
    console: { log: (...args) => messages.push(args.join(' ')), error: (...args) => messages.push(args.join(' ')) },
  });
  const dispatcher = runInContext(declarations + '\n;({ processPullRequest })', sandbox,
    { filename: 'inactive-dependabot-dispatcher.js', timeout: 5000 });
  await dispatcher.processPullRequest(root, { pullRequestNumber: 42, observedHeadSha: head });
  return { calls, messages };
}

test('dispatcher rejects otherwise valid metadata without trusted signer proof', async () => {
  const result = await simulateProcess({ armed: false });
  assert.ok(result.calls.some((call) => call.path.endsWith('/actions/workflows/123/runs')),
    'expected exact-base CI to be examined before the signer barrier');
  assert.ok(result.messages.some((message) => message.includes('signer identity cannot be independently verified')));
  assert.equal(result.calls.filter((call) => call.method !== 'GET').length, 0);
});

test('dispatcher skips existing native auto-merge without cancellation', async () => {
  const result = await simulateProcess({ armed: true });
  assert.ok(result.messages.some((message) => message.includes('Skipping an existing native auto-merge')));
  assert.equal(result.calls.filter((call) => call.method !== 'GET').length, 0);
  assert.equal(result.calls.filter((call) => call.path.endsWith('/pulls/42')).length, 2);
});
