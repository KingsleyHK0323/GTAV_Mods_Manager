// OpenAI-Codex_merge_policy_audit: Exercise the existing disabled workflow with mocked APIs.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createContext, runInContext } from 'node:vm';

const workflowPath = new URL('../workflows/dependabot-auto-merge.yml', import.meta.url);
const workflow = readFileSync(workflowPath, 'utf8');
const scripts = [...workflow.matchAll(/^[ \t]*node <<'NODE'\r?\n([\s\S]*?)^[ \t]*NODE[ \t]*\r?$/gm)];
assert.equal(scripts.length, 1, 'expected exactly one Node heredoc in the workflow');
const originalScript = scripts[0][1];
const entryPoint = 'main().catch((error) => {';
assert.equal(originalScript.split(entryPoint).length, 2, 'expected one main entry point');
const instrumentedScript = originalScript.replace(entryPoint, 'globalThis.__completion = main().catch((error) => {');

const repository = 'example/repository';
const root = `/repos/${repository}`;
const expectedSha = 'a'.repeat(40);
const changedSha = 'b'.repeat(40);
const prNumber = 42;

function response(status, body) {
  return { ok: status >= 200 && status < 300, status, text: async () => JSON.stringify(body) };
}

// All request outcomes and filesystem reads are synthetic; unmatched requests fail closed.
async function exercise(options = {}) {
  const calls = [];
  const unexpected = [];
  const messages = [];
  let pullReads = 0;
  let nativeAutoMerge = Boolean(options.nativeAutoMerge);
  const proc = {
    exitCode: 0,
    env: {
      GITHUB_API_URL: 'https://api.example.invalid',
      GITHUB_REPOSITORY: repository,
      GITHUB_EVENT_NAME: 'check_suite',
      GITHUB_EVENT_PATH: '/virtual/event.json',
      GH_TOKEN: 'synthetic-token',
    },
  };
  const event = {
    repository: { full_name: repository },
    check_suite: {
      conclusion: 'success', head_sha: expectedSha, pull_requests: [{ number: prNumber }],
    },
  };
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
    const pathname = url.pathname;
    calls.push({ method, path: pathname, query: url.search, body: init.body });
    if (url.origin !== 'https://api.example.invalid' || url.username || url.password || url.hash) {
      unexpected.push(`${method} ${url.origin}${pathname}`);
      throw new Error('unexpected network destination');
    }
    if (init.headers?.Authorization !== 'Bearer synthetic-token') {
      unexpected.push(`${method} ${pathname}: unexpected authorization`);
      throw new Error('unexpected authorization');
    }
    const fileList = method === 'GET' && pathname === `${root}/pulls/${prNumber}/files`;
    const ciList = method === 'GET' && pathname === `${root}/actions/workflows/123/runs`;
    const expectedKeys = fileList ? ['page', 'per_page'] : ciList ? ['event', 'head_sha', 'page', 'per_page'] : [];
    const actualKeys = [...url.searchParams.keys()].sort();
    if (JSON.stringify(actualKeys) !== JSON.stringify(expectedKeys) ||
        ((fileList || ciList) && (url.searchParams.get('per_page') !== '100' || !/^[1-9][0-9]*$/.test(url.searchParams.get('page') || ''))) ||
        (ciList && (url.searchParams.get('event') !== 'pull_request' || url.searchParams.get('head_sha') !== expectedSha))) {
      unexpected.push(`${method} ${pathname}: unexpected query`);
      throw new Error('unexpected query');
    }
    if (method === 'GET' && pathname === `${root}/pulls/${prNumber}`) {
      pullReads += 1;
      const sha = options.changedHeadAt && pullReads >= options.changedHeadAt ? changedSha : expectedSha;
      return response(200, {
        state: 'open', draft: false, number: prNumber, node_id: 'synthetic-node',
        title: options.title || 'Bump package from 1.0.0 to 1.0.1',
        user: { login: 'dependabot[bot]' },
        base: { repo: { full_name: repository }, ref: 'Source_Code', sha: 'c'.repeat(40) },
        head: { repo: { full_name: repository }, sha },
        auto_merge: nativeAutoMerge ? { enabled: true } : null,
      });
    }
    if (method === 'POST' && pathname === '/graphql') {
      nativeAutoMerge = false;
      return response(200, { data: { disablePullRequestAutoMerge: { pullRequest: { number: prNumber } } } });
    }
    if (method === 'GET' && pathname === `${root}/pulls/${prNumber}/files`) {
      if (options.filesError) return response(500, { message: 'synthetic error' });
      const page = Number(url.searchParams.get('page'));
      return response(200, options.filePages ? (options.filePages[page - 1] || []) : (options.files || [{ filename: 'Cargo.lock', status: 'modified' }]));
    }
    if (method === 'GET' && pathname === `${root}/actions/workflows/ci.yml`) {
      return response(200, { id: 123, name: 'CI', path: '.github/workflows/ci.yml' });
    }
    if (method === 'GET' && pathname === `${root}/actions/workflows/123/runs`) {
      const runs = options.missingCi ? [] : [{
        workflow_id: 123, event: 'pull_request', head_sha: expectedSha,
        repository: { full_name: repository }, head_repository: { full_name: repository },
        pull_requests: [{ number: prNumber }], run_number: 5, run_attempt: 1,
        status: 'completed', conclusion: options.ciConclusion || 'success',
      }];
      return response(200, { workflow_runs: runs });
    }
    if (method === 'PUT' && pathname === `${root}/pulls/${prNumber}/merge`) {
      return response(200, { merged: true });
    }
    unexpected.push(`${method} ${pathname}`);
    throw new Error('unexpected API request');
  };
  const context = createContext({
    process: proc,
    URLSearchParams,
    fetch: fakeFetch,
    require(name) {
      assert.equal(name, 'node:fs');
      return fakeFs;
    },
    console: { log: (...args) => messages.push(args.join(' ')), error: (...args) => messages.push(args.join(' ')) },
  });
  runInContext(instrumentedScript, context, { filename: 'dependabot-auto-merge.inline.js' });
  await context.__completion;
  assert.deepEqual(unexpected, []);
  return { calls, messages, exitCode: proc.exitCode };
}

function merges(result) {
  return result.calls.filter((call) => call.method === 'PUT' && call.path === `${root}/pulls/${prNumber}/merge`);
}

test('current behavior: an eligible PR with successful CI reaches only the mocked merge endpoint', async () => {
  const result = await exercise();
  assert.equal(result.exitCode, 0);
  assert.equal(merges(result).length, 1);
  assert.deepEqual(JSON.parse(merges(result)[0].body), { sha: expectedSha, merge_method: 'squash' });
});

test('current behavior: a changed head before final merge does not reach the merge endpoint', async () => {
  const result = await exercise({ changedHeadAt: 4 });
  assert.equal(result.exitCode, 0);
  assert.equal(merges(result).length, 0);
});

for (const [name, options] of [
  ['missing CI', { missingCi: true }],
  ['failed CI', { ciConclusion: 'failure' }],
  ['workflow file change', { files: [{ filename: '.github/workflows/ci.yml', status: 'modified' }] }],
  ['renamed workflow file', { files: [{ filename: 'other.yml', previous_filename: '.github/workflows/ci.yml', status: 'renamed' }] }],
]) {
  test(`current behavior: ${name} does not reach the merge endpoint`, async () => {
    const result = await exercise(options);
    assert.equal(result.exitCode, 0);
    assert.equal(merges(result).length, 0);
  });
}

test('current behavior: a workflow change on a later file page is rejected', async () => {
  const firstPage = Array.from({ length: 100 }, (_, index) => ({ filename: `docs/${index}.md` }));
  const result = await exercise({ filePages: [firstPage, [{ filename: '.github/workflows/ci.yml' }]] });
  assert.equal(result.exitCode, 0);
  assert.equal(merges(result).length, 0);
  assert.equal(result.calls.filter((call) => call.path.endsWith('/files')).length, 2);
});

test('current behavior: an API failure reports failure and does not merge', async () => {
  const result = await exercise({ filesError: true });
  assert.equal(result.exitCode, 1);
  assert.equal(merges(result).length, 0);
});

test('documented gap: a major version PR is not rejected by the current policy', async () => {
  const result = await exercise({ title: 'Bump package from 1.0.0 to 2.0.0' });
  assert.equal(result.exitCode, 0);
  assert.equal(merges(result).length, 1);
});

test('documented gap: non-workflow script changes are not rejected by the current policy', async () => {
  const result = await exercise({ files: [{ filename: '.github/scripts/prepare-release.mjs', status: 'modified' }] });
  assert.equal(result.exitCode, 0);
  assert.equal(merges(result).length, 1);
});

test('documented gap: native auto-merge is cancelled before a workflow change is rejected', async () => {
  const result = await exercise({ nativeAutoMerge: true, files: [{ filename: '.github/workflows/ci.yml' }] });
  const cancellation = result.calls.findIndex((call) => call.method === 'POST' && call.path === '/graphql');
  const files = result.calls.findIndex((call) => call.path.endsWith('/files'));
  assert.ok(cancellation >= 0 && files > cancellation);
  assert.equal(merges(result).length, 0);
  assert.equal(result.exitCode, 0);
});
