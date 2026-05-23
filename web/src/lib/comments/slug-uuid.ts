/**
 * Map a dataset slug to a stable UUID for the comments API
 * (#5a.3). The comments table keys on UUID `target_id`, but
 * the current static dataset fixture uses slug strings — this
 * helper bridges the gap until /datasets/[id] migrates to
 * the gateway DB (slot-in replacement: drop this helper, use
 * the row's actual UUID).
 *
 * RFC 4122 UUIDv5 (SHA-1 of namespace || name). Pure Node
 * crypto, server-only because of the `node:crypto` import.
 */

import { createHash } from 'node:crypto';

/**
 * Fixed namespace UUID for "dataset slugs in this repo".
 * Generated once and pinned — changing it would orphan every
 * existing comment thread. The value is the lowercase hex
 * form of a v4 UUID; any RFC-4122-conforming UUID works as a
 * namespace.
 */
const DATASET_SLUG_NAMESPACE = 'a8a3a6f4-3d51-4b3f-9b6e-8a3d8e91c0b1';

export function datasetSlugToUuid(slug: string): string {
	const nsBytes = parseUuidToBytes(DATASET_SLUG_NAMESPACE);
	const slugBytes = Buffer.from(slug, 'utf8');
	const input = Buffer.concat([nsBytes, slugBytes]);
	const hash = createHash('sha1').update(input).digest();
	const bytes = Buffer.from(hash.subarray(0, 16));
	// RFC 4122 §4.3: set version to 5 + variant to RFC 4122.
	bytes[6] = (bytes[6]! & 0x0f) | 0x50;
	bytes[8] = (bytes[8]! & 0x3f) | 0x80;
	return formatUuid(bytes);
}

function parseUuidToBytes(uuid: string): Buffer {
	const hex = uuid.replace(/-/g, '');
	if (hex.length !== 32) {
		throw new Error(`invalid namespace UUID: ${uuid}`);
	}
	return Buffer.from(hex, 'hex');
}

function formatUuid(bytes: Buffer): string {
	const hex = bytes.toString('hex');
	return [
		hex.slice(0, 8),
		hex.slice(8, 12),
		hex.slice(12, 16),
		hex.slice(16, 20),
		hex.slice(20, 32)
	].join('-');
}
