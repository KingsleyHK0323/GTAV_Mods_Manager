// OpenAI-Codex
// OpenAI-Codex_Responsible: Website
export function createStatusChangeEntry(mod, previousStatus) {
	return {
		type: 'status',
		name: mod.name,
		from: previousStatus,
		to: mod.status,
	};
}

export function createAddedEntry(mod) {
	return {
		type: 'added',
		name: mod.name,
		to: mod.status,
	};
}

export function formatHistoryEntry(entry, locale, statusLabels) {
	if (entry.type === 'status') {
		const from = statusLabels[entry.from] ?? entry.from;
		const to = statusLabels[entry.to] ?? entry.to;
		return locale === 'en'
			? `Changed ${entry.name} from ${from} to ${to}.`
			: `${entry.name}：${from} → ${to}`;
	}

	const status = statusLabels[entry.to] ?? entry.to;
	return locale === 'en'
		? `Added ${entry.name} (${status}).`
		: `新增 ${entry.name}（${status}）`;
}

