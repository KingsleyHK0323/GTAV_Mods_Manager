// OpenAI-Codex_policy_design: Synthetic tests for the inactive isolated policy candidate.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { TextDecoder } from 'node:util';
import { createContext, runInContext } from 'node:vm';

const candidatePath = new URL('../candidates/dependabot-auto-merge.candidate.yml', import.meta.url);
const candidate = readFileSync(candidatePath, 'utf8');
const scripts = [...candidate.matchAll(/^[ \t]*node <<'NODE'\r?\n([\s\S]*?)^[ \t]*NODE[ \t]*\r?$/gm)];
assert.equal(scripts.length, 1, 'candidate must contain exactly one heredoc');
assert.match(candidate, /false && \(/, 'the job must remain disabled');
const entryPoint = 'main().catch((error) => {';
assert.equal(scripts[0][1].split(entryPoint).length, 2);
const script = scripts[0][1].replace(entryPoint, 'globalThis.__completion = main().catch((error) => {');

const repository = 'example/repository';
const repositoryId = 12345;
const botId = 49699333;
const root = '/repos/' + repository;
const base = 'a'.repeat(40);
const head = 'b'.repeat(40);
const other = 'c'.repeat(40);
const baseTree = 'd'.repeat(40);
const headTree = 'e'.repeat(40);
const prNumber = 42;
const manifestPath = 'crates/gtavmm-app/package.json';
const lockPath = 'crates/gtavmm-app/package-lock.json';

function blob(text) {
  const bytes = Buffer.from(text, 'utf8');
  const sha = createHash('sha1').update('blob ' + bytes.length + '\0').update(bytes).digest('hex');
  return { type: 'file', encoding: 'base64', size: bytes.length, sha, content: bytes.toString('base64') };
}

function contents(options) {
  const oldSpec = options.oldSpec || '^1.2.3';
  const newSpec = options.newSpec || '^1.2.4';
  const oldVersion = options.oldVersion || '1.2.3';
  const newVersion = options.newVersion || '1.2.4';
  const oldManifest = { name: 'fixture', dependencies: { sample: oldSpec } };
  const newManifest = { name: 'fixture', dependencies: { sample: newSpec } };
  if (options.unsafeNumber) {
    oldManifest.probe = 9007199254740992;
    newManifest.probe = 9007199254740992;
  }
  if (options.extraDependency) {
    oldManifest.dependencies.second = '^2.3.4';
    newManifest.dependencies.second = '^2.3.5';
  }
  if (options.changedScript) newManifest.scripts = { install: 'unexpected' };
  const integrity1 = 'sha512-' + Buffer.alloc(64, 1).toString('base64');
  const integrity2 = 'sha512-' + Buffer.alloc(64, 2).toString('base64');
  const oldLock = { lockfileVersion: 3, packages: {
    '': { dependencies: { sample: oldSpec } },
    'node_modules/sample': {
      version: oldVersion,
      resolved: 'https://registry.npmjs.org/sample/-/sample-' + oldVersion + '.tgz',
      integrity: integrity1,
    },
  } };
  const newLock = JSON.parse(JSON.stringify(oldLock));
  newLock.packages[''].dependencies.sample = newSpec;
  newLock.packages['node_modules/sample'].version = newVersion;
  newLock.packages['node_modules/sample'].resolved = 'https://registry.npmjs.org/sample/-/sample-' + newVersion + '.tgz';
  newLock.packages['node_modules/sample'].integrity = integrity2;
  if (options.registryTrailing) newLock.packages['node_modules/sample'].resolved += '\n';
  if (options.badIntegrity) newLock.packages['node_modules/sample'].integrity = 'sha512-invalid';
  if (options.badRegistry) newLock.packages['node_modules/sample'].resolved = 'https://other.example/sample.tgz';
  if (options.transitiveChange) newLock.packages['node_modules/sample'].dependencies = { other: '^1.0.0' };
  if (options.lockVersion !== undefined) newLock.lockfileVersion = options.lockVersion;
  let oldManifestText = JSON.stringify(oldManifest);
  if (options.duplicateKeys) oldManifestText = oldManifestText.replace('"name":"fixture"', '"name":"fixture","name":"fixture"');
  if (options.longFraction) oldManifestText = oldManifestText.replace('"name":"fixture"', '"probe":3.0000000000000000000001,"name":"fixture"');
  if (options.negativeZero) oldManifestText = oldManifestText.replace('"name":"fixture"', '"probe":-0,"name":"fixture"');
  if (options.fraction) oldManifestText = oldManifestText.replace('"name":"fixture"', '"probe":1.5,"name":"fixture"');
  if (options.exponent) oldManifestText = oldManifestText.replace('"name":"fixture"', '"probe":1e2,"name":"fixture"');
  return {
    [base]: { [manifestPath]: blob(oldManifestText), [lockPath]: blob(JSON.stringify(oldLock)) },
    [head]: { [manifestPath]: blob(JSON.stringify(newManifest)), [lockPath]: blob(JSON.stringify(newLock)) },
  };
}

function response(status, value) {
  // OpenAI-Codex_policy_design: Add synthetic signature metadata without real API access.
  if (value?.syntheticMissingDraft || value?.syntheticMissingAutoMerge) {
    value = Object.fromEntries(Object.entries(value).filter(([key]) =>
      !(value.syntheticMissingDraft && key === 'draft') && !(value.syntheticMissingAutoMerge && key === 'auto_merge')));
  }
  if (value?.commit?.verification && value?.author?.login === 'dependabot[bot]') {
    value.committer = { login: 'dependabot[bot]', id: value.syntheticBadCommitter ? 1 : botId, type: 'Bot' };
    value.commit.verification.reason = value.syntheticBadReason ? 'invalid' : 'valid';
  }
  if (Array.isArray(value?.workflow_runs)) {
    for (const run of value.workflow_runs) {
      for (const associated of run.pull_requests || []) {
        if (associated.base && !run.syntheticMissingCiRef) associated.base.ref = run.syntheticWrongCiRef ? 'Other' : 'Source_Code';
      }
      if (run.syntheticMissingNumber) run.run_number = null;
      if (run.syntheticMissingAttempt) run.run_attempt = null;
    }
  }
  return { ok: status >= 200 && status < 300, status, text: async () => JSON.stringify(value) };
}

async function exercise(options = {}) {
  const files = contents(options);
  const calls = [];
  const unexpected = [];
  const messages = [];
  let pullReads = 0;
  let branchReads = 0;
  const fakeProcess = {
    exitCode: 0,
    env: {
      GITHUB_API_URL: 'https://api.example.invalid',
      GITHUB_REPOSITORY: repository,
      GITHUB_REPOSITORY_ID: String(repositoryId),
      GITHUB_EVENT_NAME: 'check_suite',
      GITHUB_EVENT_PATH: '/virtual/event.json',
      GH_TOKEN: 'synthetic-token',
    },
  };
  const event = { repository: { full_name: repository }, check_suite: {
    conclusion: 'success', head_sha: head, pull_requests: [{ number: prNumber }],
  } };
  const fakeFs = {
    readFileSync(path, encoding) {
      assert.equal(path, '/virtual/event.json');
      assert.equal(encoding, 'utf8');
      return JSON.stringify(event);
    },
  };

  const fakeFetch = async (input, init = {}) => {
    const url = new URL(input);
    const method = init.method || 'GET';
    const path = url.pathname;
    calls.push({ method, path, query: url.search, body: init.body });
    if (url.origin !== 'https://api.example.invalid' || url.username || url.password || url.hash ||
        init.headers?.Authorization !== 'Bearer synthetic-token') {
      unexpected.push(method + ' ' + path);
      throw new Error('Unexpected destination or authorization.');
    }
    const fileList = method === 'GET' && path === root + '/pulls/' + prNumber + '/files';
    const runs = method === 'GET' && path === root + '/actions/workflows/123/runs';
    const tree = method === 'GET' && path.startsWith(root + '/git/trees/');
    const contentFile = method === 'GET' && path.startsWith(root + '/contents/');
    const keys = [...url.searchParams.keys()].sort();
    const expected = fileList ? ['page', 'per_page'] : runs ? ['event', 'head_sha', 'page', 'per_page'] :
      tree ? ['recursive'] : contentFile ? ['ref'] : [];
    if (JSON.stringify(keys) !== JSON.stringify(expected) ||
        (fileList && (url.searchParams.get('per_page') !== '100' || url.searchParams.get('page') !== '1')) ||
        (runs && (url.searchParams.get('event') !== 'pull_request' || url.searchParams.get('head_sha') !== head ||
                  url.searchParams.get('per_page') !== '100' || url.searchParams.get('page') !== '1')) ||
        (tree && url.searchParams.get('recursive') !== '1') ||
        (contentFile && ![base, head].includes(url.searchParams.get('ref')))) {
      unexpected.push(method + ' ' + path + ': query');
      throw new Error('Unexpected query.');
    }

    if (method === 'GET' && path === root + '/pulls/' + prNumber) {
      pullReads += 1;
      const currentHead = options.changedHeadAt && pullReads >= options.changedHeadAt ? other : head;
      return response(200, {
        syntheticMissingDraft: Boolean(options.missingDraft), syntheticMissingAutoMerge: Boolean(options.missingAutoMerge),
        number: prNumber, node_id: 'synthetic-node', state: 'open', draft: false,
        changed_files: options.extraFile ? 3 : 2, commits: 1,
        user: { login: 'dependabot[bot]', id: options.badBot ? 1 : botId, type: 'Bot' },
        base: { ref: 'Source_Code', sha: base, repo: { full_name: repository, id: repositoryId } },
        head: { sha: currentHead, repo: { full_name: repository, id: repositoryId } },
        auto_merge: options.nativeAutoMerge ? { enabled: true } : null,
      });
    }
    if (method === 'GET' && path === root + '/branches/Source_Code') {
      branchReads += 1;
      const branchSha = options.changedBranchAt && branchReads >= options.changedBranchAt ? other : base;
      return response(200, { name: 'Source_Code', commit: { sha: branchSha } });
    }
    if (method === 'GET' && path === root + '/commits/' + head) {
      return response(200, {
        sha: head, author: { login: 'dependabot[bot]', id: botId, type: 'Bot' },
        syntheticBadCommitter: Boolean(options.badCommitter), syntheticBadReason: Boolean(options.badSignatureReason),
        commit: { verification: { verified: !options.unsigned }, tree: { sha: headTree } },
        parents: [{ sha: options.wrongParent ? other : base }],
      });
    }
    if (fileList) {
      if (options.filesError) return response(500, { message: 'synthetic error' });
      const first = { filename: manifestPath, status: options.deleted ? 'removed' : 'modified', sha: files[head][manifestPath].sha };
      if (options.renamed) first.previous_filename = 'other.json';
      return response(200, [first, { filename: options.unapprovedFile ? '.github/scripts/arbitrary.mjs' : lockPath,
        status: 'modified', sha: files[head][lockPath].sha }]);
    }
    if (method === 'GET' && path === root + '/git/commits/' + base) {
      return response(200, { sha: base, tree: { sha: baseTree } });
    }
    if (tree) {
      const treeSha = path.slice((root + '/git/trees/').length);
      if (![baseTree, headTree].includes(treeSha)) throw new Error('Unexpected tree ID.');
      const ref = treeSha === baseTree ? base : head;
      const entries = [manifestPath, lockPath].map((filePath) => ({
        path: filePath, mode: options.symlink && filePath === manifestPath ? '120000' : '100644',
        type: 'blob', sha: files[ref][filePath].sha,
      }));
      if (options.duplicateTree) entries.push({ ...entries[0] });
      return response(200, { sha: treeSha, truncated: Boolean(options.truncatedTree), tree: entries });
    }
    if (contentFile) {
      const filePath = path.slice((root + '/contents/').length);
      const ref = url.searchParams.get('ref');
      if (!files[ref] || !files[ref][filePath]) throw new Error('Unexpected synthetic content path.');
      if (options.badBlob && ref === head && filePath === manifestPath) {
        return response(200, { ...files[ref][filePath], sha: other });
      }
      return response(200, files[ref][filePath]);
    }
    if (method === 'GET' && path === root + '/actions/workflows/ci.yml') {
      return response(200, { id: 123, name: 'CI', path: '.github/workflows/ci.yml' });
    }
    if (runs) {
      const associated = {
        number: prNumber,
        head: { sha: head, repo: { id: repositoryId } },
        base: { sha: options.wrongCiBase ? other : base, repo: { id: repositoryId } },
      };
      if (options.missingCiBase) delete associated.base.sha;
      return response(200, { workflow_runs: options.missingCi ? [] : [{
        syntheticMissingCiRef: Boolean(options.missingCiRef), syntheticWrongCiRef: Boolean(options.wrongCiRef),
        syntheticMissingNumber: Boolean(options.missingRunNumber), syntheticMissingAttempt: Boolean(options.missingRunAttempt),
        workflow_id: 123, event: 'pull_request', head_sha: head,
        repository: { full_name: repository, id: repositoryId },
        head_repository: { full_name: repository, id: options.wrongCiRepo ? 7 : repositoryId },
        pull_requests: [associated], run_number: 5, run_attempt: 1,
        status: 'completed', conclusion: options.failedCi ? 'failure' : 'success',
      }] });
    }
    if (method === 'PUT' && path === root + '/pulls/' + prNumber + '/merge') {
      return response(200, { merged: true });
    }
    unexpected.push(method + ' ' + path);
    throw new Error('Unexpected API request.');
  };

  const context = createContext({
    process: fakeProcess, URLSearchParams, URL, Buffer, TextDecoder, fetch: fakeFetch,
    require(name) {
      if (name === 'node:fs') return fakeFs;
      if (name === 'node:crypto') return { createHash };
      throw new Error('Unexpected require: ' + name);
    },
    console: { log: (...args) => messages.push(args.join(' ')), error: (...args) => messages.push(args.join(' ')) },
  });
  runInContext(script, context, { filename: 'inactive-dependabot-candidate.js', timeout: 5000 });
  await context.__completion;
  assert.deepEqual(unexpected, []);
  return { calls, messages, exitCode: fakeProcess.exitCode };
}

function mutations(result) {
  return result.calls.filter((call) => call.method !== 'GET');
}

// OpenAI-Codex_policy_design: This earlier candidate assertion is preserved unchanged.
// It now conflicts with the signer-identity fail-closed guard and is expected to fail
// until the user permits replacing it; it is not a test-specific safety bypass.
test('a narrowly valid patch reaches only a synthetic merge after complete checks', async () => {
  const result = await exercise();
  assert.equal(result.exitCode, 0);
  assert.equal(mutations(result).length, 1);
  assert.equal(mutations(result)[0].method, 'PUT');
  assert.deepEqual(JSON.parse(mutations(result)[0].body), { sha: head, merge_method: 'squash' });
  assert.ok(result.calls.some((call) => call.path.endsWith('/git/trees/' + baseTree)));
  assert.ok(result.calls.some((call) => call.path.endsWith('/git/trees/' + headTree)));
});

for (const [name, options] of [
  ['major version', { newSpec: '^2.0.0', newVersion: '2.0.0' }],
  ['minor version', { newSpec: '^1.3.0', newVersion: '1.3.0' }],
  ['zero major', { oldSpec: '^0.2.3', newSpec: '^0.2.4', oldVersion: '0.2.3', newVersion: '0.2.4' }],
  ['prerelease', { newSpec: '^1.2.4-beta.1' }],
  ['wildcard', { newSpec: '^1.2' }],
  ['range change', { newSpec: '~1.2.4' }],
  ['unsafe integer', { newSpec: '^1.2.9007199254740992' }],
  ['grouped updates', { extraDependency: true }],
  ['extra file', { extraFile: true }],
  ['unapproved file', { unapprovedFile: true }],
  ['removed file', { deleted: true }],
  ['renamed file', { renamed: true }],
  ['untrusted bot identity', { badBot: true }],
  ['unsigned head commit', { unsigned: true }],
  ['head is not a direct child of base', { wrongParent: true }],
  ['missing CI', { missingCi: true }],
  ['failed CI', { failedCi: true }],
  ['different CI base', { wrongCiBase: true }],
  ['missing CI base', { missingCiBase: true }],
  ['different CI repository', { wrongCiRepo: true }],
  ['changed branch before mutation', { changedBranchAt: 2 }],
  ['changed head before mutation', { changedHeadAt: 3 }],
  ['truncated Git tree', { truncatedTree: true }],
  ['duplicate tree path', { duplicateTree: true }],
  ['symlink instead of regular file', { symlink: true }],
  ['duplicate JSON keys', { duplicateKeys: true }],
  ['invalid integrity', { badIntegrity: true }],
  ['untrusted registry', { badRegistry: true }],
  ['transitive lockfile change', { transitiveChange: true }],
  ['unsupported lockfile version', { lockVersion: 2 }],
  ['existing native auto-merge', { nativeAutoMerge: true }],
  ['version with trailing newline', { newSpec: '^1.2.4\n' }],
  ['registry URL with trailing newline', { registryTrailing: true }],
  ['rounded fractional JSON number', { longFraction: true }],
  ['unsafe JSON integer', { unsafeNumber: true }],
  ['negative zero JSON number', { negativeZero: true }],
  ['fractional JSON number', { fraction: true }],
  ['exponential JSON number', { exponent: true }],
  ['untrusted commit committer', { badCommitter: true }],
  ['invalid signature reason', { badSignatureReason: true }],
  ['missing CI run number', { missingRunNumber: true }],
  ['missing CI attempt', { missingRunAttempt: true }],
  ['content blob identity mismatch', { badBlob: true }],
  ['missing draft field', { missingDraft: true }],
  ['missing native auto-merge field', { missingAutoMerge: true }],
  ['missing CI base ref', { missingCiRef: true }],
  ['different CI base ref', { wrongCiRef: true }],
  ['file list API error', { filesError: true }],
]) {
  test('rejects ' + name + ' before cancellation or merge', async () => {
    const result = await exercise(options);
    // OpenAI-Codex_policy_design: A rejection case must not pass only because of the signer barrier.
    assert.equal(result.messages.some((message) => message.includes('signer identity cannot be independently verified')), false);
    // OpenAI-Codex_policy_design: Require the intended rejection path, not a swallowed exception.
    assert.equal(result.exitCode, options.filesError ? 1 : 0);
    const reason = options.nativeAutoMerge ? 'Skipping an existing native auto-merge' :
      options.filesError ? 'GitHub API request failed with HTTP 500' : 'conservative policy was not satisfied';
    assert.ok(result.messages.some((message) => message.includes(reason)), 'expected specific preflight rejection');
    if (options.newSpec || options.oldSpec || options.extraDependency || options.changedScript ||
        options.unsafeNumber || options.negativeZero || options.fraction || options.exponent ||
        options.longFraction || options.duplicateKeys || options.badIntegrity ||
        options.badRegistry || options.registryTrailing || options.transitiveChange || options.lockVersion !== undefined) {
      assert.ok(result.calls.some((call) => call.path.endsWith('/contents/' + manifestPath)), 'expected metadata read');
    }
    if (options.missingCi || options.failedCi || options.wrongCiBase || options.missingCiBase ||
        options.wrongCiRepo || options.wrongCiRef || options.missingCiRef ||
        options.missingRunNumber || options.missingRunAttempt) {
      assert.ok(result.calls.some((call) => call.path.endsWith('/actions/workflows/123/runs')), 'expected CI run read');
    }
    if (options.truncatedTree || options.duplicateTree || options.symlink) {
      assert.ok(result.calls.some((call) => call.path.includes('/git/trees/')), 'expected tree read');
    }
    assert.equal(mutations(result).length, 0);
  });
}

// OpenAI-Codex_policy_design: A synthetically valid patch still cannot prove the bot signer.
test('otherwise valid synthetic metadata cannot authorize any mutation without trusted signer proof', async () => {
  const result = await exercise();
  assert.equal(mutations(result).length, 0);
  assert.ok(result.messages.some((message) => message.includes('signer identity cannot be independently verified')));
});

// OpenAI-Codex_policy_design: Test metadata predicates independently without invoking main or any API.
test('strict metadata can describe a patch while signer proof remains unavailable', () => {
  const sample = contents({});
  const decode = (entry) => JSON.parse(Buffer.from(entry.content, 'base64').toString('utf8'));
  const context = createContext({
    URLSearchParams, URL, Buffer, TextDecoder,
    process: { env: { GITHUB_API_URL: 'https://api.example.invalid', GITHUB_REPOSITORY: repository, GH_TOKEN: 'synthetic-token' } },
    require(name) {
      if (name === 'node:fs') return { readFileSync() { throw new Error('Unexpected file read.'); } };
      if (name === 'node:crypto') return { createHash };
      throw new Error('Unexpected require: ' + name);
    },
    fetch() { throw new Error('Unexpected network request.'); },
    console: { log() {}, error() {} },
    fixture: {
      oldManifest: decode(sample[base][manifestPath]),
      newManifest: decode(sample[head][manifestPath]),
      oldLock: decode(sample[base][lockPath]),
      newLock: decode(sample[head][lockPath]),
    },
  });
  const marker = 'globalThis.__completion = main().catch((error) => {';
  const offset = script.indexOf(marker);
  assert.ok(offset > 0);
  runInContext(script.slice(0, offset), context, { filename: 'inactive-dependabot-predicates.js', timeout: 5000 });
  assert.equal(runInContext('policyLockMatches(fixture.oldLock, fixture.newLock, policyOneDependency(fixture.oldManifest, fixture.newManifest))', context), true);
  // OpenAI-Codex_policy_design: Directly exercise policy predicates independently of the signer barrier.
  assert.equal(runInContext('policyVersion("^1.2.4").bare', context), '1.2.4');
  for (const version of ['^2.0.0\n', '^0.1.2', '^1.2', 'latest', '^1.2.4-beta', '^1.2.9007199254740992']) {
    context.fixture.version = version;
    assert.equal(runInContext('policyVersion(fixture.version)', context), null);
  }
  assert.equal(runInContext('policyRegistryUrl("https://registry.npmjs.org/sample/-/sample-1.2.4.tgz", "sample", "1.2.4")', context), true);
  assert.equal(runInContext('policyRegistryUrl("https://registry.npmjs.org/sample/-/sample-1.2.4.tgz\\n", "sample", "1.2.4")', context), false);
  for (const raw of ['{"a":1,"a":2}', '{"n":-0}', '{"n":1e2}', '{"n":3.0000000000000000000001}', '{"n":9007199254740992}']) {
    context.fixture.raw = raw;
    assert.throws(() => runInContext('policyUniqueJson(fixture.raw)', context));
  }
  const badLock = JSON.parse(JSON.stringify(context.fixture.newLock));
  badLock.packages['node_modules/sample'].dependencies = { unexpected: '^1.0.0' };
  context.fixture.badLock = badLock;
  assert.equal(runInContext('policyLockMatches(fixture.oldLock, fixture.badLock, policyOneDependency(fixture.oldManifest, fixture.newManifest))', context), false);
  context.process.env.GITHUB_REPOSITORY_ID = String(repositoryId);
  context.fixture.pr = {
    number: prNumber, state: 'open', draft: false, auto_merge: null,
    user: { login: 'dependabot[bot]', id: botId, type: 'Bot' },
    base: { ref: 'Source_Code', sha: base, repo: { full_name: repository, id: repositoryId } },
    head: { sha: head, repo: { full_name: repository, id: repositoryId } },
  };
  context.fixture.head = head;
  context.fixture.base = base;
  context.fixture.prNumber = prNumber;
  assert.equal(runInContext('policySameIdentity(fixture.pr, fixture.prNumber, fixture.head, fixture.base)', context), true);
  context.fixture.pr.draft = undefined;
  assert.equal(runInContext('policySameIdentity(fixture.pr, fixture.prNumber, fixture.head, fixture.base)', context), false);
  context.fixture.pr.draft = false;
  context.fixture.pr.auto_merge = undefined;
  assert.equal(runInContext('policySameIdentity(fixture.pr, fixture.prNumber, fixture.head, fixture.base)', context), false);
  assert.equal(runInContext('policyTrustedDependabotSigner()', context), false);
});
