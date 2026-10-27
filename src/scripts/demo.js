// OpenAI-Codex
// OpenAI-Codex_Responsible: Website
import { createAddedEntry, createStatusChangeEntry, formatHistoryEntry } from './demo-history.js';

const sampleMods = [
	{ id: 'sample-01', name: 'Night Lighting Pack', category: 'graphics', language: 'multi', status: 'enabled' },
	{ id: 'sample-02', name: 'Harbor Weather Set', category: 'environment', language: 'en', status: 'disabled' },
	{ id: 'sample-03', name: 'Coastal Detail Kit', category: 'world', language: 'zh-TW', status: 'enabled' },
];

const copy = {
	'en': {
		categories: { graphics: 'Graphics', environment: 'Environment', world: 'World' },
		languages: { multi: 'Multiple', en: 'English', 'zh-TW': 'Traditional Chinese' },
		status: { enabled: 'Enabled', disabled: 'Disabled' },
		actions: { enable: 'Enable', disable: 'Disable', options: 'Options' },
		filter: { all: 'All items', enabled: 'Enabled', disabled: 'Disabled' },
		empty: 'No sample items match your search or status filter.',
		emptyAction: 'Clear search and filter',
		count: { total: 'sample items', enabled: 'enabled', disabled: 'disabled' },
		menuTitle: 'Actions for ',
		detailsTitle: 'Details for ',
		details: (mod) => `${mod.name} is a sample ${category(mod.category, 'en').toLowerCase()} item in ${language(mod.language, 'en')}. The demo does not load or save any files.`,
		toggled: (mod) => `${mod.name} is now ${status(mod.status, 'en').toLowerCase()} in this demo.`,
		added: 'Harbor Weather Set was added to this page’s sample list.',
		reset: 'The sample list has been reset.',
		history: { title: 'Session activity', empty: 'No actions yet.', clear: 'Clear history', cleared: 'The activity history was cleared.' },
	},
	'zh-TW': {
		categories: { graphics: '畫面', environment: '環境', world: '世界' },
		languages: { multi: '多語言', en: '英文', 'zh-TW': '繁體中文' },
		status: { enabled: '已啟用', disabled: '已停用' },
		actions: { enable: '啟用', disable: '停用', options: '選項' },
		filter: { all: '所有項目', enabled: '已啟用', disabled: '已停用' },
		empty: '沒有符合搜尋或狀態篩選條件的範例項目。',
		emptyAction: '清除搜尋與篩選',
		count: { total: '個範例項目', enabled: '個已啟用', disabled: '個已停用' },
		menuTitle: '項目操作：',
		detailsTitle: '項目詳細資料：',
		details: (mod) => `${mod.name} 是一個「${category(mod.category, 'zh-TW')}」範例項目，語言為${language(mod.language, 'zh-TW')}。Demo 不會載入或儲存任何檔案。`,
		toggled: (mod) => `${mod.name} 在此 Demo 中已${status(mod.status, 'zh-TW')}。`,
		added: 'Harbor Weather Set 已加入目前頁面的範例清單。',
		reset: '範例清單已重設。',
		history: { title: '本次操作摘要', empty: '目前尚無操作。', clear: '清除記錄', cleared: '操作記錄已清除。' },
	},
};

function category(value, locale) {
	return copy[locale].categories[value] ?? value;
}

function language(value, locale) {
	return copy[locale].languages[value] ?? value;
}

function status(value, locale) {
	return copy[locale].status[value] ?? value;
}

function initDemo(root) {
	const locale = root.dataset.locale === 'en' ? 'en' : 'zh-TW';
	const strings = copy[locale];
	let mods = sampleMods.map((mod) => ({ ...mod }));
	let historyEntries = [];
	let selectedId = null;
	let nextSampleId = 4;
	let restoreFocusTo = null;

	const list = root.querySelector('[data-demo-list]');
	const search = root.querySelector('[data-demo-search]');
	const filter = root.querySelector('[data-demo-filter]');
	const contextMenu = root.querySelector('[data-context-menu]');
	const installDialog = root.querySelector('[data-install-dialog]');
	const detailsDialog = root.querySelector('[data-details-dialog]');
	const announcement = root.querySelector('[data-demo-announcement]');
	const historyList = root.querySelector('[data-demo-history-list]');
	const historyEmpty = root.querySelector('[data-demo-history-empty]');
	const historyClear = root.querySelector('[data-demo-history-clear]');
	const selectedMod = () => mods.find((mod) => mod.id === selectedId);

	function visibleMods() {
		const query = search.value.trim().toLocaleLowerCase(locale);
		return mods.filter((mod) => {
			const matchesStatus = filter.value === 'all' || mod.status === filter.value;
			const matchesSearch = !query || `${mod.name} ${category(mod.category, locale)}`.toLocaleLowerCase(locale).includes(query);
			return matchesStatus && matchesSearch;
		});
	}

	function updateSummary() {
		const enabled = mods.filter((mod) => mod.status === 'enabled').length;
		const disabled = mods.length - enabled;
		root.querySelector('[data-summary-total]').textContent = String(mods.length);
		root.querySelector('[data-summary-enabled]').textContent = String(enabled);
		root.querySelector('[data-summary-disabled]').textContent = String(disabled);
		root.querySelector('[data-summary-total-label]').textContent = strings.count.total;
		root.querySelector('[data-summary-enabled-label]').textContent = strings.count.enabled;
		root.querySelector('[data-summary-disabled-label]').textContent = strings.count.disabled;
	}

	function renderHistory() {
		const hasEntries = historyEntries.length > 0;
		historyList.hidden = !hasEntries;
		historyEmpty.hidden = hasEntries;
		historyClear.disabled = !hasEntries;
		historyList.replaceChildren();

		for (const entry of historyEntries) {
			const item = document.createElement('li');
			item.textContent = formatHistoryEntry(entry, locale, strings.status);
			historyList.append(item);
		}
	}

	function makeCell(text) {
		const cell = document.createElement('td');
		cell.textContent = text;
		return cell;
	}

	function render() {
		const visible = visibleMods();
		list.replaceChildren();

		if (visible.length === 0) {
			const row = document.createElement('tr');
			const cell = document.createElement('td');
			cell.colSpan = 4;
			const emptyState = document.createElement('div');
			emptyState.className = 'demo-empty-state';
			const message = document.createElement('span');
			message.textContent = strings.empty;
			const clearFilters = document.createElement('button');
			clearFilters.type = 'button';
			clearFilters.className = 'demo-button';
			clearFilters.dataset.demoClearFilters = '';
			clearFilters.textContent = strings.emptyAction;
			emptyState.append(message, clearFilters);
			cell.append(emptyState);
			row.append(cell);
			list.append(row);
		} else {
			for (const mod of visible) {
				const row = document.createElement('tr');
				row.dataset.modId = mod.id;
				row.tabIndex = 0;
				row.dataset.selected = String(mod.id === selectedId);

				const nameCell = document.createElement('td');
				const name = document.createElement('span');
				name.className = 'mod-name';
				name.textContent = mod.name;
				const categoryLabel = document.createElement('span');
				categoryLabel.className = 'mod-category';
				categoryLabel.textContent = category(mod.category, locale);
				nameCell.append(name, categoryLabel);

				const languageCell = makeCell(language(mod.language, locale));
				const statusCell = document.createElement('td');
				const statusLabel = document.createElement('span');
				statusLabel.className = 'mod-status';
				statusLabel.dataset.status = mod.status;
				statusLabel.textContent = status(mod.status, locale);
				statusCell.append(statusLabel);

				const actionsCell = document.createElement('td');
				const toggle = document.createElement('button');
				toggle.type = 'button';
				toggle.className = 'demo-button';
				toggle.dataset.demoToggle = '';
				toggle.setAttribute('aria-label', `${mod.name}: ${mod.status === 'enabled' ? strings.actions.disable : strings.actions.enable}`);
				toggle.textContent = mod.status === 'enabled' ? strings.actions.disable : strings.actions.enable;
				const options = document.createElement('button');
				options.type = 'button';
				options.className = 'demo-button';
				options.dataset.demoOptions = '';
				options.textContent = strings.actions.options;
				actionsCell.append(toggle, document.createTextNode(' '), options);

				row.append(nameCell, languageCell, statusCell, actionsCell);
				list.append(row);
			}
		}

		for (const option of filter.options) {
			option.textContent = strings.filter[option.value] ?? option.value;
		}
		updateSummary();
		renderHistory();
	}

	function closeContextMenu(restoreFocus = true) {
		contextMenu.hidden = true;
		if (restoreFocus && restoreFocusTo?.isConnected) restoreFocusTo.focus();
	}

	function openContextMenu(mod, left, top, focusTarget) {
		selectedId = mod.id;
		restoreFocusTo = focusTarget;
		const toggle = root.querySelector('[data-menu-toggle]');
		toggle.textContent = mod.status === 'enabled' ? strings.actions.disable : strings.actions.enable;
		contextMenu.setAttribute('aria-label', `${strings.menuTitle}${mod.name}`);
		contextMenu.hidden = false;
		for (const row of list.querySelectorAll('tr[data-mod-id]')) {
			row.dataset.selected = String(row.dataset.modId === selectedId);
		}
		const menuWidth = contextMenu.offsetWidth;
		const menuHeight = contextMenu.offsetHeight;
		contextMenu.style.left = `${Math.max(8, Math.min(left, window.innerWidth - menuWidth - 8))}px`;
		contextMenu.style.top = `${Math.max(8, Math.min(top, window.innerHeight - menuHeight - 8))}px`;
		root.querySelector('[data-menu-toggle]').focus();
	}

	function toggleMod(id, focusRow = false) {
		const mod = mods.find((item) => item.id === id);
		if (!mod) return;
		const previousStatus = mod.status;
		mod.status = mod.status === 'enabled' ? 'disabled' : 'enabled';
		historyEntries.push(createStatusChangeEntry(mod, previousStatus));
		announcement.textContent = strings.toggled(mod);
		render();
		if (focusRow) list.querySelector(`tr[data-mod-id="${CSS.escape(id)}"]`)?.focus();
	}

	function showDetails() {
		const mod = selectedMod();
		if (!mod) return;
		closeContextMenu(false);
		root.querySelector('[data-details-copy]').textContent = strings.details(mod);
		detailsDialog.showModal();
	}

	list.addEventListener('click', (event) => {
		const target = event.target instanceof HTMLElement ? event.target : null;
		if (target?.closest('[data-demo-clear-filters]')) {
			search.value = '';
			filter.value = 'all';
			render();
			search.focus();
			return;
		}
		const row = target?.closest('tr[data-mod-id]');
		if (!row) return;
		const id = row.dataset.modId;
		selectedId = id;

		if (target.closest('[data-demo-toggle]')) {
			toggleMod(id, true);
			return;
		}
		if (target.closest('[data-demo-options]')) {
			const bounds = target.getBoundingClientRect();
			openContextMenu(mods.find((mod) => mod.id === id), bounds.left, bounds.bottom + 4, target);
			return;
		}
		render();
	});

	list.addEventListener('contextmenu', (event) => {
		const row = event.target instanceof HTMLElement ? event.target.closest('tr[data-mod-id]') : null;
		if (!row) return;
		event.preventDefault();
		const mod = mods.find((item) => item.id === row.dataset.modId);
		openContextMenu(mod, event.clientX, event.clientY, row);
	});

	list.addEventListener('keydown', (event) => {
		if (!(event.target instanceof HTMLElement)) return;
		const row = event.target.closest('tr[data-mod-id]');
		if (!row || !((event.shiftKey && event.key === 'F10') || event.key === 'ContextMenu')) return;
		event.preventDefault();
		const mod = mods.find((item) => item.id === row.dataset.modId);
		openContextMenu(mod, row.getBoundingClientRect().left, row.getBoundingClientRect().bottom, row);
	});

	contextMenu.addEventListener('click', (event) => {
		const target = event.target instanceof HTMLElement ? event.target.closest('button[role="menuitem"]') : null;
		if (!target) return;
		if (target.matches('[data-menu-toggle]')) {
			const id = selectedId;
			closeContextMenu(false);
			toggleMod(id, true);
		} else if (target.matches('[data-menu-details]')) {
			showDetails();
		}
	});

	contextMenu.addEventListener('keydown', (event) => {
		const items = [...contextMenu.querySelectorAll('[role="menuitem"]')];
		const current = items.indexOf(document.activeElement);
		if (event.key === 'Escape') {
			event.preventDefault();
			closeContextMenu();
		} else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
			event.preventDefault();
			const direction = event.key === 'ArrowDown' ? 1 : -1;
			items[(current + direction + items.length) % items.length]?.focus();
		} else if (event.key === 'Tab') {
			closeContextMenu(false);
		}
	});

	document.addEventListener('pointerdown', (event) => {
		if (!contextMenu.hidden && !contextMenu.contains(event.target)) closeContextMenu(false);
	});

	search.addEventListener('input', render);
	filter.addEventListener('change', render);
	root.querySelector('[data-demo-install]').addEventListener('click', () => installDialog.showModal());
	root.querySelector('[data-install-cancel]').addEventListener('click', () => installDialog.close());
	root.querySelector('[data-install-confirm]').addEventListener('click', () => {
		const mod = { id: `sample-${nextSampleId++}`, name: 'Harbor Weather Set', category: 'environment', language: 'multi', status: 'disabled' };
		mods.push(mod);
		historyEntries.push(createAddedEntry(mod));
		installDialog.close();
		announcement.textContent = strings.added;
		render();
	});
	root.querySelector('[data-demo-reset]').addEventListener('click', () => {
		mods = sampleMods.map((mod) => ({ ...mod }));
		historyEntries = [];
		selectedId = null;
		nextSampleId = 4;
		search.value = '';
		filter.value = 'all';
		announcement.textContent = strings.reset;
		render();
	});
	root.querySelector('[data-demo-history-clear]').addEventListener('click', () => {
		historyEntries = [];
		renderHistory();
		announcement.textContent = strings.history.cleared;
	});
	root.querySelector('[data-details-close]').addEventListener('click', () => detailsDialog.close());
	detailsDialog.addEventListener('close', () => {
		if (restoreFocusTo?.isConnected) restoreFocusTo.focus();
	});

	render();
}

document.querySelectorAll('[data-demo-root]').forEach(initDemo);

