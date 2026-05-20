<!--
	Single source of truth for page SEO metadata. Renders:
	  - <title>
	  - meta description
	  - canonical link
	  - Open Graph (og:*) tags for Facebook / LinkedIn / Discord
	  - Twitter Card meta tags
	  - JSON-LD structured data (schema.org)

	Every route that wants SEO should mount exactly one <MetaTags>
	in its <svelte:head> output. The component reads `page.url` from
	`$app/state` so the canonical URL and og:url are always correct
	for the current request — pages just supply title + description
	+ schema type.

	Validation target: Google Rich Results test
	(https://search.google.com/test/rich-results). The JSON-LD shapes
	used here (`WebSite`, `CollectionPage`, `Dataset`) are all
	recognised by Google's crawler.
-->
<script lang="ts">
	import { page } from '$app/state';

	export type SchemaType = 'WebSite' | 'CollectionPage' | 'Dataset';

	type Props = {
		/**
		 * Page-level title. Used verbatim for og:title and twitter:title
		 * (social cards already render the site domain alongside, so the
		 * shorter form looks cleaner). The browser <title> appends the
		 * site name via `fullTitle` below so tabs get site context.
		 */
		title: string;
		/** Used for meta description, og:description, twitter:description. */
		description: string;
		/** schema.org @type that best describes the page. */
		schemaType?: SchemaType;
		/** Optional absolute URL of a representative image (>= 1200×630 ideal). */
		image?: string;
		/** Optional: when present, page is hidden from search indexes. */
		noindex?: boolean;
	};

	let { title, description, schemaType = 'WebSite', image, noindex = false }: Props = $props();

	const SITE_NAME = 'Taiwan Data Hub';

	// `page.url` is reactive; deriving these keeps canonical + og:url
	// correct as the user navigates (Svelte's $derived re-evaluates on
	// each route change).
	//
	// Canonical strips query string and fragment by design: UTM tags,
	// tracking params, and #anchors would otherwise spawn dozens of
	// canonical variants per route and dilute SEO ranking signal.
	// Paginated list pages (where `?page=2` IS a distinct indexable
	// URL) can extend this component later with an explicit prop.
	const canonical = $derived(`${page.url.origin}${page.url.pathname}`);
	const fullTitle = $derived(title === SITE_NAME ? title : `${title} · ${SITE_NAME}`);

	// Escape `<` to its JSON unicode form so the stringified payload
	// can never close the surrounding <script> tag even if a future
	// caller passes user-controlled content into title/description.
	// OWASP-recommended pattern for inlining JSON in HTML.
	const jsonLd = $derived(
		JSON.stringify({
			'@context': 'https://schema.org',
			'@type': schemaType,
			name: title,
			description,
			url: canonical,
			...(image ? { image } : {})
		}).replaceAll('<', '\\u003c')
	);
</script>

<svelte:head>
	<title>{fullTitle}</title>
	<meta name="description" content={description} />
	<link rel="canonical" href={canonical} />
	{#if noindex}
		<meta name="robots" content="noindex, nofollow" />
	{/if}

	<meta property="og:type" content="website" />
	<meta property="og:title" content={title} />
	<meta property="og:description" content={description} />
	<meta property="og:url" content={canonical} />
	<meta property="og:site_name" content={SITE_NAME} />
	{#if image}
		<meta property="og:image" content={image} />
	{/if}

	<meta name="twitter:card" content={image ? 'summary_large_image' : 'summary'} />
	<meta name="twitter:title" content={title} />
	<meta name="twitter:description" content={description} />
	{#if image}
		<meta name="twitter:image" content={image} />
	{/if}

	<svelte:element this={'script'} type="application/ld+json">{jsonLd}</svelte:element>
</svelte:head>
