// OpenAI-Codex
// OpenAI-Codex_Responsible: Website
// Anthropic-Claude_Code_Responsible: Website
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import starlight from '@astrojs/starlight';

const site = 'https://kingsleyhk0323.github.io';
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
					href: 'https://github.com/KingsleyHK0323/GTAV_Mods_Manager',
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
						{ slug: 'language-conversion', label: '模組翻譯', translations: { en: 'Mod Translation' } },
						{ slug: 'managing-mods', label: '管理已安裝模組', translations: { en: 'Managing Installed Mods' } },
						{ slug: 'download', label: '下載與驗證', translations: { en: 'Download and Verify' } },
						{ slug: 'supported-modes', label: '支援模式與限制', translations: { en: 'Supported Modes and Limits' } },
						{ slug: 'safety', label: '安全機制', translations: { en: 'Safety Measures' } },
						{ slug: 'privacy', label: '隱私與資料流向', translations: { en: 'Privacy and Data Flows' } },
						{ slug: 'licenses', label: '授權與聲明', translations: { en: 'Licenses and Notices' } },
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
