<!--
	Global footer — license, repo, contact. Stacked on mobile, three
	columns on md+. Sits below the main column in +layout.svelte and is
	pushed to the bottom of short pages by `flex-1` on <main>.

	Uses design tokens from #2.1; no raw color literals.
-->
<footer class="border-t border-neutral-200 bg-neutral-50">
	<div class="mx-auto max-w-7xl px-4 py-10 sm:px-6 lg:px-8">
		<div class="grid grid-cols-1 gap-8 md:grid-cols-3">
			<section aria-labelledby="footer-license">
				<h2 id="footer-license" class="text-sm font-semibold text-neutral-900">License</h2>
				<p class="mt-2 text-sm text-neutral-600">
					Open source under
					<a
						class="text-primary-700 hover:underline"
						href="https://github.com/hydai/taiwan-data-hub/blob/main/LICENSE">Apache-2.0</a
					>.
				</p>
			</section>

			<section aria-labelledby="footer-source">
				<h2 id="footer-source" class="text-sm font-semibold text-neutral-900">Source</h2>
				<p class="mt-2 text-sm text-neutral-600">
					<a
						class="text-primary-700 hover:underline"
						href="https://github.com/hydai/taiwan-data-hub">github.com/hydai/taiwan-data-hub</a
					>
				</p>
			</section>

			<section aria-labelledby="footer-contact">
				<h2 id="footer-contact" class="text-sm font-semibold text-neutral-900">Contact</h2>
				<p class="mt-2 text-sm text-neutral-600">
					Bug reports and feature requests via
					<a
						class="text-primary-700 hover:underline"
						href="https://github.com/hydai/taiwan-data-hub/issues">GitHub Issues</a
					>.
				</p>
			</section>
		</div>
		<div class="mt-10 border-t border-neutral-200 pt-6 text-xs text-neutral-500">
			© 2026 Taiwan Data Hub contributors — fully open source, self-hostable, no client-side
			tracking.
		</div>
	</div>
</footer>
