// OpenAI-Codex
// OpenAI-Codex_Responsible: Website
import { defineRouteMiddleware } from '@astrojs/starlight/route-data';

const site = import.meta.env.SITE ?? 'https://spacesquare640.github.io';
const base = import.meta.env.BASE_URL.replace(/\/$/, '');
const managedProperties = new Set([
	'og:title',
	'og:type',
	'og:description',
	'og:url',
	'og:locale',
	'og:locale:alternate',
	'og:image:alt',
	'twitter:title',
	'twitter:description',
	'twitter:image:alt',
]);

function getPagePath(pathname: string) {
	let path = pathname;
	if (base && (path === base || path.startsWith(`${base}/`))) {
		path = path.slice(base.length) || '/';
	}
	path = path.replace(/index\.html$/, '');
	if (!path.startsWith('/')) path = `/${path}`;
	if (!path.endsWith('/')) path = `${path}/`;
	return path;
}

function absolutePageUrl(path: string) {
	return new URL(`${base}${path}`, site).href;
}

function meta(property: string, content: string) {
	return { tag: 'meta', attrs: { property, content } };
}

function namedMeta(name: string, content: string) {
	return { tag: 'meta', attrs: { name, content } };
}

export const onRequest = defineRouteMiddleware((context) => {
	const route = context.locals.starlightRoute;
	const path = getPagePath(context.url.pathname);
	const canonical = absolutePageUrl(path);
	const isEnglish = route.lang.toLowerCase().startsWith('en');
	const title = route.entry.data.title || route.siteTitle;
	const description = route.entry.data.description?.trim();
	const locale = isEnglish ? 'en_US' : 'zh_TW';
	const imageAlt = isEnglish
		? 'GTAV Mods Manager orange gear and V mark over a night-time city.'
		: 'GTAV Mods Manager 橘色齒輪與 V 標誌，搭配夜色城市背景。';
	const englishPath = isEnglish ? path : `/en${path === '/' ? '/' : path}`;
	const chinesePath = isEnglish ? path.slice('/en'.length) || '/' : path;
	const otherLocale = isEnglish ? 'zh_TW' : 'en_US';

	const retainedHead = route.head.filter((tag) => {
		if (tag.tag === 'link' && tag.attrs?.rel === 'canonical') return false;
		if (tag.tag === 'link' && tag.attrs?.rel === 'alternate' && tag.attrs?.hreflang) return false;
		if (tag.tag !== 'meta') return true;
		const property = tag.attrs?.property;
		const name = tag.attrs?.name;
		return !(
			(typeof property === 'string' && managedProperties.has(property)) ||
			(typeof name === 'string' && managedProperties.has(name))
		);
	});
	route.head.splice(0, route.head.length, ...retainedHead);
	if (path.endsWith('/404/')) {
		if (!route.head.some((tag) => tag.tag === 'meta' && tag.attrs?.name === 'robots')) {
			route.head.push(namedMeta('robots', 'noindex'));
		}
		return;
	}
	route.head.push(
		{ tag: 'link', attrs: { rel: 'canonical', href: canonical } },
		{ tag: 'link', attrs: { rel: 'alternate', hreflang: 'zh-TW', href: absolutePageUrl(chinesePath) } },
		{ tag: 'link', attrs: { rel: 'alternate', hreflang: 'en', href: absolutePageUrl(englishPath) } },
		meta('og:title', title),
		meta('og:type', path === '/' || path === '/en/' ? 'website' : 'article'),
		meta('og:description', description ?? title),
		meta('og:url', canonical),
		meta('og:locale', locale),
		meta('og:locale:alternate', otherLocale),
		meta('og:image:alt', imageAlt),
		namedMeta('twitter:title', title),
		namedMeta('twitter:description', description ?? title),
		namedMeta('twitter:image:alt', imageAlt),
	);
});
