<!--
	Global footer — license, repo, contact. Stacked on mobile, three
	columns on md+. Sits below the main column in +layout.svelte and is
	pushed to the bottom of short pages by `flex-1` on <main>.

	Uses design tokens from #2.1; no raw color literals.
-->
<script lang="ts">
	import { m } from '$lib/paraglide/messages';

	// Project's first publication year. The displayed range auto-extends
	// to the current year so the copyright stays accurate without manual
	// updates. Same year on both endpoints collapses to a single year.
	const COPYRIGHT_START_YEAR = 2026;
	const currentYear = new Date().getFullYear();
	const copyrightYears =
		currentYear === COPYRIGHT_START_YEAR
			? `${COPYRIGHT_START_YEAR}`
			: `${COPYRIGHT_START_YEAR}–${currentYear}`;
</script>

<footer class="border-t border-neutral-200 bg-neutral-50">
	<div class="mx-auto max-w-7xl px-4 py-10 sm:px-6 lg:px-8">
		<div class="grid grid-cols-1 gap-8 md:grid-cols-3">
			<section aria-labelledby="footer-license">
				<h2 id="footer-license" class="text-sm font-semibold text-neutral-900">
					{m.footer_license()}
				</h2>
				<p class="mt-2 text-sm text-neutral-600">
					{m.footer_license_prefix()}
					<a
						class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
						href="https://github.com/hydai/taiwan-data-hub/blob/main/LICENSE">Apache-2.0</a
					>{m.footer_license_suffix()}
				</p>
			</section>

			<section aria-labelledby="footer-source">
				<h2 id="footer-source" class="text-sm font-semibold text-neutral-900">
					{m.footer_source()}
				</h2>
				<p class="mt-2 text-sm text-neutral-600">
					<a
						class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
						href="https://github.com/hydai/taiwan-data-hub">github.com/hydai/taiwan-data-hub</a
					>
				</p>
			</section>

			<section aria-labelledby="footer-contact">
				<h2 id="footer-contact" class="text-sm font-semibold text-neutral-900">
					{m.footer_contact()}
				</h2>
				<p class="mt-2 text-sm text-neutral-600">
					{m.footer_contact_prefix()}
					<a
						class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
						href="https://github.com/hydai/taiwan-data-hub/issues">GitHub Issues</a
					>{m.footer_contact_suffix()}
				</p>
			</section>
		</div>
		<div class="mt-10 border-t border-neutral-200 pt-6 text-xs text-neutral-500">
			© {copyrightYears}
			{m.footer_copyright_suffix()}
		</div>
	</div>
</footer>
