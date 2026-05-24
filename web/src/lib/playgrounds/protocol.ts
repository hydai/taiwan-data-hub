/**
 * postMessage protocol between the framing page (parent) and the
 * sandboxed playground iframe (child). Centralised here so both
 * sides stay in lockstep — the parent rejects any unknown message
 * type, the child only ever sends one of these shapes.
 *
 * Design notes:
 *
 *  - **Unique request ids** for the API-proxy round-trip. A
 *    playground can fire several `tdh.fetch(...)` in flight; the
 *    parent uses the `id` to match each response to the right
 *    promise on the child side.
 *
 *  - **No raw `Response` shuttled across the boundary** — structured-
 *    clone of `Response` is unreliable across browsers and would
 *    leak shape Sandboxed code shouldn't rely on. Instead the parent
 *    sends a small DTO (`{ ok, status, body, contentType }`) and the
 *    child rebuilds a real `Response` locally.
 *
 *  - **Gateway-path discipline at the parent**. The child cannot
 *    fetch arbitrary URLs; the parent validates `path` against an
 *    allowlist (currently `^/api/v1/`) before proxying. The
 *    sandbox's unique-opaque-origin already prevents direct
 *    cross-origin fetches, but the allowlist defends against
 *    same-origin path traversal (e.g. a playground trying to
 *    proxy through to `/admin`).
 */

/** Discriminated message-type tags. */
export const PLAYGROUND_MSG_TYPES = {
	/** Child → parent: shim has loaded, ready to receive INIT. */
	READY: 'tdh:ready',
	/** Parent → child: hand the initial decoded state (or null). */
	INIT: 'tdh:init',
	/** Child → parent: a new state value to encode into the URL. */
	STATE_CHANGED: 'tdh:state-changed',
	/** Child → parent: please run this fetch against the gateway. */
	API_CALL: 'tdh:api-call',
	/** Parent → child: result of an api-call, keyed by the call's id. */
	API_RESPONSE: 'tdh:api-response'
} as const;

export type PlaygroundMsgType = (typeof PLAYGROUND_MSG_TYPES)[keyof typeof PLAYGROUND_MSG_TYPES];

export interface ReadyMessage {
	type: typeof PLAYGROUND_MSG_TYPES.READY;
}

export interface InitMessage {
	type: typeof PLAYGROUND_MSG_TYPES.INIT;
	state: unknown;
}

export interface StateChangedMessage {
	type: typeof PLAYGROUND_MSG_TYPES.STATE_CHANGED;
	state: unknown;
}

export interface ApiCallMessage {
	type: typeof PLAYGROUND_MSG_TYPES.API_CALL;
	id: string;
	path: string;
	init?: {
		method?: string;
		headers?: Record<string, string>;
		body?: string;
	};
}

export interface ApiResponseMessage {
	type: typeof PLAYGROUND_MSG_TYPES.API_RESPONSE;
	id: string;
	ok: boolean;
	status: number;
	contentType: string | null;
	body: string;
	/** Non-empty when the call was rejected by the parent (allowlist, network failure). */
	error?: string;
}

export type PlaygroundChildToParent = ReadyMessage | StateChangedMessage | ApiCallMessage;
export type PlaygroundParentToChild = InitMessage | ApiResponseMessage;

/**
 * Allowlist for `tdh.fetch` paths. The parent rejects anything that
 * doesn't begin with this prefix; the prefix is also documented in
 * `playgrounds/README.md`. Centralised here so a future move (e.g.
 * `/v2/api/...`) is a one-line change that updates both the parent
 * validator and any tooling that wants to assert the rule.
 */
export const PLAYGROUND_GATEWAY_PREFIX = '/api/v1/';
