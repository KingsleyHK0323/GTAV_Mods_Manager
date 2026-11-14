// OpenAI-Codex_release_impl: Mock the original-run and artifact provenance checks.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { verifyProvenance, verifyInstallerDigests, BUILD_SHA, RUN_ID, ARTIFACT_ID, ZIP_SHA256, TAG } from './publish-beta-recovery.mjs';

function fixture() {
  return {
    run: { id: RUN_ID, head_sha: BUILD_SHA, head_branch: TAG, event: 'push', status: 'completed' },
    jobs: ['Validate beta release tag and versions', 'test-gate / build-and-test (windows-latest)', 'build-windows']
      .map((name) => ({ name, run_id: RUN_ID, conclusion: 'success' })),
    artifact: { id: ARTIFACT_ID, name: 'installers-windows', expired: false, digest: 'sha256:' + ZIP_SHA256,
      workflow_run: { id: RUN_ID, head_sha: BUILD_SHA } },
  };
}

test('recovery requires the reviewed source, successful gates and exact artifact', () => {
  const good = fixture();
  assert.doesNotThrow(() => verifyProvenance(good.run, good.jobs, good.artifact));
  assert.throws(() => verifyProvenance({ ...good.run, head_sha: 'a'.repeat(40) }, good.jobs, good.artifact));
  assert.throws(() => verifyProvenance({ ...good.run, head_branch: 'another-tag' }, good.jobs, good.artifact));
  assert.throws(() => verifyProvenance(good.run, good.jobs.slice(1), good.artifact));
  assert.throws(() => verifyProvenance(good.run, [...good.jobs, good.jobs[0]], good.artifact));
  assert.throws(() => verifyProvenance(good.run, good.jobs, { ...good.artifact, expired: true }));
  assert.throws(() => verifyProvenance(good.run, good.jobs, { ...good.artifact, id: 1 }));
  assert.throws(() => verifyProvenance(good.run, good.jobs, { ...good.artifact, digest: 'sha256:' + '0'.repeat(64) }));
});

test('checksum must contain exactly the nonempty Windows installers and their digests', () => {
  const installer = { name: 'GTAV Mods Manager_1.0.0_x64_en-US.msi', size: 3, sha256: '903da42569003df651c54b207a4985c9ebaacf68172a1fe2a917ab820ffebd8d' };
  const second = { name: 'GTAV Mods Manager_1.0.0_x64-setup.exe', size: 3, sha256: '33e6650c646c4f5f663bc24eea4f1dbd127174ad94b07f94b4d02dbe9a9fc930' };
  const data = Buffer.from(installer.sha256 + '  ' + installer.name + '\n' + second.sha256 + '  ' + second.name + '\n');
  const checksum = { name: 'SHA256SUMS-windows.txt', data, size: data.length,
    sha256: createHash('sha256').update(data).digest('hex') };
  assert.doesNotThrow(() => verifyInstallerDigests([installer, second, checksum]));
  assert.throws(() => verifyInstallerDigests([]));
  assert.throws(() => verifyInstallerDigests([checksum]));
  assert.throws(() => verifyInstallerDigests([installer]));
  assert.throws(() => verifyInstallerDigests([installer, installer, checksum]));
  assert.throws(() => verifyInstallerDigests([installer, second, checksum, checksum]));
  assert.throws(() => verifyInstallerDigests([installer, { ...installer, name: 'other.zip' }, checksum]));
  assert.throws(() => verifyInstallerDigests([installer, second, { ...checksum, sha256: '0'.repeat(64) }]));
  assert.throws(() => verifyInstallerDigests([{ ...installer, size: 0 }, second, checksum]));
  assert.throws(() => verifyInstallerDigests([{ ...installer, name: 'setup.deb' }, second, checksum]));
  assert.throws(() => verifyInstallerDigests([{ ...installer, sha256: 'b'.repeat(64) }, second, checksum]));
  assert.throws(() => verifyInstallerDigests([installer, second, { ...checksum, data: Buffer.from('tampered') }]));
});
