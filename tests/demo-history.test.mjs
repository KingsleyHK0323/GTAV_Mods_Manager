// OpenAI-Codex
// OpenAI-Codex_Responsible: Website
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const historyHelpers = await import('../src/scripts/demo-history.js').catch(() => null);

test('status history captures the state before and after a change', () => {
	assert.equal(typeof historyHelpers?.createStatusChangeEntry, 'function', 'createStatusChangeEntry should be exported');
	const entry = historyHelpers.createStatusChangeEntry(
		{ name: 'Night Lighting Pack', status: 'disabled' },
		'enabled',
	);
	assert.deepEqual(entry, {
		type: 'status',
		name: 'Night Lighting Pack',
		from: 'enabled',
		to: 'disabled',
	});
});

test('sample additions record their new state without a previous state', () => {
	assert.equal(typeof historyHelpers?.createAddedEntry, 'function', 'createAddedEntry should be exported');
	assert.deepEqual(
		historyHelpers.createAddedEntry({ name: 'Harbor Weather Set', status: 'disabled' }),
		{ type: 'added', name: 'Harbor Weather Set', to: 'disabled' },
	);
});

test('history entries have localized Traditional Chinese and English summaries', () => {
	assert.equal(typeof historyHelpers?.formatHistoryEntry, 'function', 'formatHistoryEntry should be exported');
	const entry = { type: 'status', name: 'Night Lighting Pack', from: 'enabled', to: 'disabled' };
	assert.equal(
		historyHelpers.formatHistoryEntry(entry, 'en', { enabled: 'Enabled', disabled: 'Disabled' }),
		'Changed Night Lighting Pack from Enabled to Disabled.',
	);
	assert.equal(
		historyHelpers.formatHistoryEntry(entry, 'zh-TW', { enabled: '已啟用', disabled: '已停用' }),
		'Night Lighting Pack：已啟用 → 已停用',
	);
	const added = historyHelpers.createAddedEntry({ name: 'Harbor Weather Set', status: 'disabled' });
	assert.equal(
		historyHelpers.formatHistoryEntry(added, 'en', { disabled: 'Disabled' }),
		'Added Harbor Weather Set (Disabled).',
	);
	assert.equal(
		historyHelpers.formatHistoryEntry(added, 'zh-TW', { disabled: '已停用' }),
		'新增 Harbor Weather Set（已停用）',
	);
});

test('the demo connects bilingual history controls to its responsive panel', async () => {
	const [demoSource, componentSource, styleSource] = await Promise.all([
		readFile(new URL('../src/scripts/demo.js', import.meta.url), 'utf8'),
		readFile(new URL('../src/components/DemoWorkspace.astro', import.meta.url), 'utf8'),
		readFile(new URL('../src/styles/site.css', import.meta.url), 'utf8'),
	]);
	assert.match(componentSource, /data-demo-history-list/);
	assert.match(componentSource, /data-demo-history-clear/);
	assert.match(componentSource, /本次操作摘要/);
	assert.match(componentSource, /Session activity/);
	assert.match(demoSource, /createStatusChangeEntry/);
	assert.match(demoSource, /createAddedEntry/);
	assert.match(demoSource, /historyEntries\.push\(createStatusChangeEntry\(mod, previousStatus\)\)/);
	assert.match(demoSource, /historyEntries\.push\(createAddedEntry\(mod\)\)/);
	assert.equal((demoSource.match(/historyEntries = \[\]/g) ?? []).length, 3, 'initialization, clear, and reset empty the history');
	assert.match(demoSource, /item\.textContent = formatHistoryEntry/);
	assert.doesNotMatch(demoSource, /\.innerHTML\b/);
	assert.match(demoSource, /data-demo-history-clear/);
	assert.match(demoSource, /updateSummary\(\);\s*renderHistory\(\);/);
	assert.match(styleSource, /\.demo-history\b/);
	assert.match(styleSource, /\.demo-history-heading\s*\{[^}]*flex-wrap: wrap/s);
});

