// OpenAI-Codex_identity_options: Observe event provenance without authorizing a merge.
import { readFile } from 'node:fs/promises';

const botId = 49699333;
const botLogin = 'dependabot[bot]';
const repo = 'KingsleyHK0323/GTAV_Mods_Manager';
const branch = 'Source_Code';
const shaPattern = /^[0-9a-f]{40}$/;
const positiveInteger = (value) => Number.isSafeInteger(value) && value > 0;
const validSha = (value) => typeof value === 'string' && shaPattern.test(value);

function unknown(reason) {
  console.log(`Event evidence: unknown (${reason}).`);
  process.exitCode = 1;
}

console.log('Event evidence is not signer verification or a merge authorization.');
if (process.env.GITHUB_EVENT_NAME !== 'pull_request') {
  console.log('Event evidence: not applicable (not a pull request event).');
} else {
  try {
    const event = JSON.parse(await readFile(process.env.GITHUB_EVENT_PATH, 'utf8'));
    const pr = event.pull_request;
    const author = pr?.user;
    if (!positiveInteger(author?.id) || typeof author?.login !== 'string') {
      unknown('missing pull request author');
    } else if (author.id !== botId && author.login !== botLogin) {
      console.log('Event evidence: not applicable (pull request author is not Dependabot).');
    } else if (author.id !== botId || author.login !== botLogin) {
      unknown('inconsistent pull request author');
    } else if (!positiveInteger(pr.number) || event.number !== pr.number ||
        event.repository?.full_name !== repo || pr.base?.repo?.full_name !== repo ||
        pr.head?.repo?.full_name !== repo || pr.base?.ref !== branch ||
        !validSha(pr.head?.sha) || !validSha(pr.base?.sha)) {
      unknown('incomplete event identity or revision');
    } else if (!['opened', 'synchronize', 'reopened'].includes(event.action)) {
      unknown('event action does not attest a revision update');
    } else if (!positiveInteger(event.sender?.id) || typeof event.sender?.login !== 'string' ||
        !/^[1-9][0-9]*$/.test(process.env.GITHUB_ACTOR_ID ?? '') ||
        !Number.isSafeInteger(Number(process.env.GITHUB_ACTOR_ID))) {
      unknown('missing event sender or actor');
    } else if (event.sender.id !== botId || event.sender.login !== botLogin ||
        Number(process.env.GITHUB_ACTOR_ID) !== botId || process.env.GITHUB_ACTOR !== botLogin) {
      unknown('event was not initiated by the Dependabot identity');
    } else {
      const response = await fetch(`https://api.github.com/repos/${repo}/pulls/${pr.number}`, {
        method: 'GET',
        redirect: 'error',
        headers: { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' },
        signal: AbortSignal.timeout(15000),
      });
      if (!response.ok) throw new Error('Public API unavailable');
      const current = await response.json();
      if (current.number !== pr.number || current.state !== 'open' ||
          current.user?.id !== botId || current.user?.login !== botLogin ||
          current.base?.repo?.full_name !== repo || current.head?.repo?.full_name !== repo ||
          current.base?.ref !== branch || current.head?.sha !== pr.head.sha ||
          current.base?.sha !== pr.base.sha) {
        unknown('event is stale or current pull request differs');
      } else {
        console.log(`Event evidence: sender and actor matched Dependabot for PR #${pr.number}.`);
        console.log(`Observed event head=${pr.head.sha} base=${pr.base.sha}.`);
        console.log('This records event attribution only; it does not verify commit provenance or signer.');
      }
    }
  } catch {
    unknown('event data or public API unavailable');
  }
}
