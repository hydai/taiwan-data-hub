#!/usr/bin/env node
// i18n coverage check (#7.10)
//
// Compares each locale's message catalog against the zh-TW source of
// truth. Per the DESIGN.md DoD:
//   - en MUST be 100% — missing keys exit non-zero (blocks CI)
//   - ja / ko / fr are advisory — missing keys print warnings only
//
// `$schema` is a meta key (Inlang format hint) and excluded from both
// the source set and per-locale counts. Extra keys (present in a
// locale but not in zh-TW) are surfaced too — they're orphans from a
// renamed/removed source key and always wrong to leave dangling.

import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const MESSAGES_DIR = join(SCRIPT_DIR, '..', 'messages');
const SOURCE_LOCALE = 'zh-TW';
const HARD_FAIL_LOCALES = new Set(['en']);
const META_KEYS = new Set(['$schema']);

function loadCatalog(locale) {
	const path = join(MESSAGES_DIR, `${locale}.json`);
	const raw = readFileSync(path, 'utf8');
	const obj = JSON.parse(raw);
	const keys = new Set(Object.keys(obj).filter((k) => !META_KEYS.has(k)));
	return { path, keys };
}

function diff(source, target) {
	const missing = [...source].filter((k) => !target.has(k)).sort();
	const extra = [...target].filter((k) => !source.has(k)).sort();
	return { missing, extra };
}

function discoverLocales() {
	return readdirSync(MESSAGES_DIR)
		.filter((f) => f.endsWith('.json'))
		.map((f) => f.replace(/\.json$/, ''))
		.filter((l) => l !== SOURCE_LOCALE)
		.sort();
}

const source = loadCatalog(SOURCE_LOCALE);
console.log(`i18n coverage check — source: ${SOURCE_LOCALE} (${source.keys.size} keys)`);
console.log('');

const discovered = discoverLocales();
// Belt-and-braces: a deleted or renamed messages/<hard-fail>.json
// would silently drop the locale from `discovered`, skipping its
// gate. Assert presence up-front so the failure is loud and obvious.
const missingHardFail = [...HARD_FAIL_LOCALES].filter((l) => !discovered.includes(l));
if (missingHardFail.length > 0) {
	console.error(
		`✗ Hard-fail locale(s) have no catalog file: ${missingHardFail.join(', ')}. ` +
			`Restore web/messages/<locale>.json or update HARD_FAIL_LOCALES.`
	);
	process.exit(1);
}

let hardErrors = 0;
let warnings = 0;
const summary = [];

for (const locale of discovered) {
	const target = loadCatalog(locale);
	const { missing, extra } = diff(source.keys, target.keys);
	const coverage =
		source.keys.size === 0 ? 100 : ((source.keys.size - missing.length) / source.keys.size) * 100;
	const hard = HARD_FAIL_LOCALES.has(locale);
	const level = missing.length === 0 && extra.length === 0 ? 'ok' : hard ? 'error' : 'warn';
	summary.push({ locale, coverage, missing: missing.length, extra: extra.length, level });

	if (missing.length === 0 && extra.length === 0) continue;

	const tag = hard ? 'ERROR' : 'WARN';
	console.log(`[${tag}] ${locale}: ${coverage.toFixed(1)}% coverage`);
	if (missing.length > 0) {
		console.log(`  missing (${missing.length}):`);
		for (const k of missing) console.log(`    - ${k}`);
	}
	if (extra.length > 0) {
		console.log(`  extra/orphaned (${extra.length}):`);
		for (const k of extra) console.log(`    + ${k}`);
	}
	console.log('');

	if (hard && (missing.length > 0 || extra.length > 0)) hardErrors += 1;
	else if (!hard && (missing.length > 0 || extra.length > 0)) warnings += 1;
}

console.log('Summary');
console.log('-------');
for (const s of summary) {
	const status = s.level === 'ok' ? '✓' : s.level === 'warn' ? '⚠' : '✗';
	console.log(
		`${status} ${s.locale.padEnd(6)} ${s.coverage.toFixed(1).padStart(5)}% coverage  ` +
			`missing=${s.missing}  extra=${s.extra}`
	);
}
console.log('');

const hardFailLocales = [...HARD_FAIL_LOCALES].sort().join(', ');
if (hardErrors > 0) {
	console.error(
		`✗ ${hardErrors} hard-fail locale(s) failed coverage. ${hardFailLocales} MUST be 100%.`
	);
	process.exit(1);
}
if (warnings > 0) {
	const advisoryLocales = summary
		.filter((s) => s.level === 'warn')
		.map((s) => s.locale)
		.join(', ');
	console.log(`⚠ ${warnings} advisory locale(s) have gaps (${advisoryLocales}).`);
}
console.log('✓ i18n coverage check passed.');
