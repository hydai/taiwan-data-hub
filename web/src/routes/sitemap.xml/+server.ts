import { loadDomainGroups } from '$lib/domains/load';
import type { RequestHandler } from './$types';

/**
 * Top-level routes that aren't generated from data. Add new top-level
 * pages here so they appear in the sitemap.
 *
 * `lastmod` is omitted intentionally — without per-page change-tracking
 * the values would be either wrong (today's date for everything) or
 * static (the build date), neither of which gives crawlers useful
 * signal. Re-add per-page lastmod once we have content metadata
 * (e.g., from #2.6 dataset detail).
 */
const STATIC_ROUTES: readonly { path: string; priority: number; changefreq: string }[] = [
	{ path: '/', priority: 1.0, changefreq: 'weekly' },
	{ path: '/domains', priority: 0.9, changefreq: 'weekly' },
	{ path: '/datasets', priority: 0.9, changefreq: 'daily' },
	{ path: '/collections', priority: 0.8, changefreq: 'weekly' }
];

/**
 * sitemap.xml — protocol per https://www.sitemaps.org/protocol.html.
 *
 * The DoD calls for "split into chunks if > 50k URLs". We're at ~24
 * now; when we cross 40k or so the right move is to switch this
 * endpoint to a sitemap-index that points at multiple sub-sitemaps
 * grouped by route family (one for /domains/[slug], one per N
 * thousand /datasets/[id], etc.). The data-loading layer
 * (`loadDomainGroups` etc.) is already grouped that way so the
 * migration will be mechanical.
 */
export const GET: RequestHandler = ({ url, setHeaders }) => {
	const origin = url.origin;
	const domainSlugs = loadDomainGroups().flatMap((g) => g.domains.map((d) => d.slug));

	const urls: { loc: string; priority?: number; changefreq?: string }[] = [
		...STATIC_ROUTES.map((r) => ({
			loc: `${origin}${r.path}`,
			priority: r.priority,
			changefreq: r.changefreq
		})),
		...domainSlugs.map((slug) => ({
			loc: `${origin}/domains/${slug}`,
			priority: 0.7,
			changefreq: 'weekly'
		}))
	];

	const xml = renderSitemap(urls);

	setHeaders({
		'content-type': 'application/xml; charset=utf-8',
		'cache-control': 'public, max-age=86400'
	});

	return new Response(xml);
};

function renderSitemap(
	urls: readonly { loc: string; priority?: number; changefreq?: string }[]
): string {
	const body = urls
		.map((u) => {
			const fields = [
				`    <loc>${escapeXml(u.loc)}</loc>`,
				u.changefreq ? `    <changefreq>${u.changefreq}</changefreq>` : null,
				u.priority !== undefined ? `    <priority>${u.priority.toFixed(1)}</priority>` : null
			]
				.filter(Boolean)
				.join('\n');
			return `  <url>\n${fields}\n  </url>`;
		})
		.join('\n');

	return `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${body}
</urlset>
`;
}

/** Minimal XML escaping for safe text inside `<loc>`. */
function escapeXml(value: string): string {
	return value
		.replaceAll('&', '&amp;')
		.replaceAll('<', '&lt;')
		.replaceAll('>', '&gt;')
		.replaceAll('"', '&quot;')
		.replaceAll("'", '&apos;');
}
