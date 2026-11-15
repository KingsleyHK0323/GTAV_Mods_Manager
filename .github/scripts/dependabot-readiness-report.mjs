// OpenAI-Codex_report_impl: Report public observations only; no merge decisions.
const origin = 'https://api.github.com';
const repository = '/repos/KingsleyHK0323/GTAV_Mods_Manager';
const baseBranch = 'Source_Code';
const maxPages = 10;

async function readPage(path) {
  const response = await fetch(`${origin}${repository}${path}`, {
    method: 'GET',
    redirect: 'error',
    headers: { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' },
    signal: AbortSignal.timeout(15000),
  });
  if (!response.ok) throw new Error(`GitHub API returned HTTP ${response.status}`);
  return response.json();
}

async function openPulls() {
  const pulls = [];
  for (let page = 1; page <= maxPages; page++) {
    const data = await readPage(`/pulls?state=open&base=${encodeURIComponent(baseBranch)}&per_page=100&page=${page}`);
    if (!Array.isArray(data)) throw new Error('Unexpected pull request response');
    pulls.push(...data);
    if (data.length < 100) return pulls;
  }
  throw new Error('Pull request pagination limit reached');
}

async function ciStatus(pr) {
  const sha = pr.head?.sha;
  if (!/^[a-f0-9]{40}$/.test(sha ?? '')) return 'unknown: invalid head SHA';
  if (!/^[a-f0-9]{40}$/.test(pr.base?.sha ?? '')) return 'unknown: invalid base SHA';
  const data = await readPage(`/actions/workflows/ci.yml/runs?event=pull_request&head_sha=${sha}&per_page=100`);
  if (!Array.isArray(data.workflow_runs) || typeof data.total_count !== 'number') return 'unknown: unexpected CI response';
  if (data.total_count > data.workflow_runs.length) return 'unknown: CI results truncated';
  const runs = data.workflow_runs.filter(run => run.head_sha === sha && Array.isArray(run.pull_requests) &&
    run.pull_requests.some(item => item.number === pr.number && item.head?.sha === sha && item.base?.sha === pr.base?.sha));
  if (!runs.length) return 'unknown: no CI run matching PR, head and base';
  runs.sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)) || b.id - a.id);
  const run = runs[0];
  if (!Number.isSafeInteger(run.id)) return 'unknown: invalid CI run ID';
  const active = new Set(['queued', 'in_progress', 'waiting', 'requested', 'pending']);
  if (active.has(run.status)) return `unknown: CI in progress; run ${run.id}`;
  const conclusions = new Set(['success', 'failure', 'neutral', 'cancelled', 'skipped', 'timed_out', 'action_required', 'stale', 'startup_failure']);
  if (run.status !== 'completed' || !conclusions.has(run.conclusion)) return `unknown: CI status or conclusion unavailable; run ${run.id}`;
  return `completed/${run.conclusion}; run ${run.id}`;
}

console.log('Public Dependabot PR observations; identity, protection, and merge readiness are not verified.');
console.log('CI below reports only runs matched by PR number, head SHA and base SHA; it does not verify the PR merge-commit check.');
try {
  const pulls = await openPulls();
  const candidates = pulls.filter(pr => pr.user?.login === 'dependabot[bot]' && pr.user?.id === 49699333);
  console.log(`Observed ${candidates.length} open Dependabot-attributed PR(s) targeting ${baseBranch}.`);
  for (const pr of candidates) {
    if (!Number.isSafeInteger(pr.number) || pr.number < 1) {
      console.error('Report incomplete: invalid pull request number.');
      process.exitCode = 1;
      continue;
    }
    const head = /^[a-f0-9]{40}$/.test(pr.head?.sha ?? '') ? pr.head.sha : 'unknown';
    const base = /^[a-f0-9]{40}$/.test(pr.base?.sha ?? '') ? pr.base.sha : 'unknown';
    let ci;
    try { ci = await ciStatus(pr); } catch { ci = 'unknown: public API request failed'; }
    if (ci.startsWith('unknown:')) process.exitCode = 1;
    console.log(`#${pr.number} head=${head} base=${base} CI=${ci}`);
  }
  if (process.exitCode) console.error('Report incomplete: one or more observations are unknown.');
} catch {
  console.error('Report incomplete: public API request, response, or pagination limit failed. State is unknown.');
  process.exitCode = 1;
}
