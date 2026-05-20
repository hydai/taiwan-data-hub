import type { RequestHandler } from './$types';

/**
 * AI/search crawler user-agents we explicitly welcome. Listed in their
 * own blocks so the policy is easy to audit and tighten later.
 *
 * NOTE: omitting a UA does not block it — the wildcard `User-agent: *`
 * block below already governs anything not named. The point of these
 * explicit allow blocks is documentation: a contributor reading the
 * file sees who we expect to crawl us. The `Disallow:` lines under
 * `*` (which apply to these UAs too) deliberately cover admin /
 * dashboard / account so even welcomed bots stay out of those.
 */
const WELCOMED_AI_BOTS = ['ClaudeBot', 'GPTBot', 'Google-Extended', 'PerplexityBot'];
const WELCOMED_SEARCH_BOTS = ['Googlebot', 'Bingbot', 'DuckDuckBot'];

const DISALLOWED_PATHS = ['/admin', '/dashboard', '/account'];

/**
 * /robots.txt — protocol per https://www.robotstxt.org/.
 *
 * Built dynamically so the Sitemap: URL adapts to whatever origin
 * the gateway is deployed under (this project is self-hostable;
 * baking in a domain would be wrong for anything but our own demo
 * instance).
 */
export const GET: RequestHandler = ({ url, setHeaders }) => {
	const lines: string[] = [];

	lines.push('# Taiwan Data Hub — robots.txt');
	lines.push('# Open source: https://github.com/hydai/taiwan-data-hub');
	lines.push('');

	lines.push('User-agent: *');
	for (const path of DISALLOWED_PATHS) {
		lines.push(`Disallow: ${path}`);
	}
	lines.push('');

	for (const bot of [...WELCOMED_AI_BOTS, ...WELCOMED_SEARCH_BOTS]) {
		lines.push(`User-agent: ${bot}`);
		for (const path of DISALLOWED_PATHS) {
			lines.push(`Disallow: ${path}`);
		}
		lines.push('Allow: /');
		lines.push('');
	}

	lines.push(`Sitemap: ${url.origin}/sitemap.xml`);

	setHeaders({
		'content-type': 'text/plain; charset=utf-8',
		'cache-control': 'public, max-age=86400'
	});

	return new Response(`${lines.join('\n')}\n`);
};
