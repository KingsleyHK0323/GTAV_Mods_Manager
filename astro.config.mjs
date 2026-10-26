// OpenAI-Codex
// OpenAI-Codex_Responsible: Website
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import starlight from '@astrojs/starlight';

const site = 'https://spacesquare640.github.io';
const base = '/GTAV_Mods_Manager';
const socialPreview = new URL(`${base}/assets/social-preview.jpg`, site).href;

export default defineConfig({
	site,
	base,
	trailingSlash: 'always',
	output: 'static',
	integrations: [
		sitemap({
			i18n: {
				defaultLocale: 'root',
				locales: { root: 'zh-TW', en: 'en' },
			},
		}),
		starlight({
			title: 'GTAV Mods Manager',
			defaultLocale: 'root',
			locales: {
				root: { label: '繁體中文', lang: 'zh-TW' },
				en: { label: 'English', lang: 'en' },
			},
			logo: {
				src: './src/assets/app-icon.jpg',
				alt: 'GTAV Mods Manager',
			},
			favicon: '/assets/app-icon.jpg',
			routeMiddleware: './src/routeData.ts',
			social: [
				{
					icon: 'discord',
					label: 'Discord',
					href: 'https://discord.gg/aaUQVJeCgC',
				},
				{
					icon: 'github',
					label: 'GitHub',
					href: 'https://github.com/SpaceSquare640/GTAV_Mods_Manager',
				},
			],
			sidebar: [
				{
					label: '使用指南',
					translations: { en: 'Guides' },
					items: [
						{ slug: 'demo', label: '互動 Demo', translations: { en: 'Interactive Demo' } },
						{ slug: 'quick-start', label: '快速開始', translations: { en: 'Quick Start' } },
						{ slug: 'installing-mods', label: '安裝模組', translations: { en: 'Installing Mods' } },
						{ slug: 'language-conversion', label: '語言轉換', translations: { en: 'Language Changes' } },
						{ slug: 'managing-mods', label: '管理已安裝模組', translations: { en: 'Managing Installed Mods' } },
						{ slug: 'faq', label: '常見問題', translations: { en: 'FAQ' } },
					],
				},
			],
			customCss: ['./src/styles/site.css'],
			head: [
				{ tag: 'meta', attrs: { property: 'og:image', content: socialPreview } },
				{ tag: 'meta', attrs: { name: 'twitter:image', content: socialPreview } },
			],
		}),
	],
});
