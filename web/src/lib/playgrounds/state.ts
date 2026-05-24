/**
 * URL-safe base64 encoder/decoder for share-link state.
 *
 * Why URL-safe? Raw base64 contains `+`, `/`, and `=` — all
 * legal in a query string but `=` needs escaping when the value is
 * inside `application/x-www-form-urlencoded`. URL-safe variant
 * (RFC 4648 §5) swaps `+`→`-`, `/`→`_`, and strips `=` padding,
 * giving a value that's safe to drop into the query string raw.
 *
 * Why JSON? Structured clone over postMessage would also work, but
 * the state value lives in the URL — JSON is the only sensible
 * serialisation for that use case anyway, and using it on both
 * sides means "what's in the URL" matches "what was passed to
 * `tdh.setState`" byte-for-byte after a round trip.
 *
 * Size cap: 2 KiB (encoded). Anything larger should live server-
 * side; URLs over ~8 KiB get truncated by intermediaries. The cap
 * keeps surprises out of share links.
 */
export const MAX_ENCODED_STATE_BYTES = 2048;

export class StateTooLargeError extends Error {
	constructor(actual: number) {
		super(
			`Playground state too large for share link: ${actual} bytes encoded ` +
				`(max ${MAX_ENCODED_STATE_BYTES}). Trim the state or persist it server-side.`
		);
		this.name = 'StateTooLargeError';
	}
}

export function encodeState(value: unknown): string {
	const json = JSON.stringify(value);
	const encoded = base64UrlEncode(json);
	if (encoded.length > MAX_ENCODED_STATE_BYTES) {
		throw new StateTooLargeError(encoded.length);
	}
	return encoded;
}

/**
 * Returns `null` when the input is empty / unparseable so callers
 * can fall back to a default state without try/catch noise. We
 * swallow malformed input on purpose: a share link copy-paste
 * truncation should land the user on the default view, not an
 * error page.
 */
export function decodeState(encoded: string | null | undefined): unknown {
	if (!encoded) return null;
	try {
		const json = base64UrlDecode(encoded);
		return JSON.parse(json);
	} catch {
		return null;
	}
}

function base64UrlEncode(input: string): string {
	const bytes = new TextEncoder().encode(input);
	// btoa needs a binary string; build one by mapping each byte
	// through fromCharCode. The byte→char round-trip is lossless
	// because we control the input via TextEncoder.
	let binary = '';
	for (const b of bytes) {
		binary += String.fromCharCode(b);
	}
	return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

function base64UrlDecode(input: string): string {
	const padLen = (4 - (input.length % 4)) % 4;
	const padded = input.replaceAll('-', '+').replaceAll('_', '/') + '='.repeat(padLen);
	const binary = atob(padded);
	const bytes = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) {
		bytes[i] = binary.charCodeAt(i);
	}
	return new TextDecoder().decode(bytes);
}
