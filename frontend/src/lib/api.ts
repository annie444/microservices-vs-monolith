// Thin client for the article API (monolith and microservices share the contract).

export type Author = { id: string; name: string; pronouns: string | null };
export type Profile = {
	id: string;
	name: string;
	pronouns: string | null;
	bio: string | null;
	followers_count: number;
	following: boolean;
};
export type Article = {
	id: string;
	creator: Author;
	title: string;
	subtitle: string | null;
	content: string;
	created_at: string;
	updated_at: string;
	tags: string[];
	favorites: number;
	favorited: boolean;
};
export type Pagination = { next_cursor: string; has_more: boolean; size: number };
export type Feed = { articles: Omit<Article, 'content'>[]; pagination: Pagination };
export type Comment = {
	id: string;
	article_id: string;
	author: Author;
	content: string;
	created_at: string;
	updated_at: string;
};
export type Comments = { comments: Comment[]; pagination: Pagination };

export class ApiError extends Error {
	constructor(
		public status: number,
		message: string,
	) {
		super(message);
	}
}

const BASE = '/api/v1';

/**
 * JSON call. Sessions last 15 minutes and refresh cookies 7 days, so a 401 is retried once
 * after `POST /users/token`; a second 401 is a real "not signed in".
 */
export async function api<T = void>(
	path: string,
	init: RequestInit & { json?: unknown } = {},
): Promise<T> {
	const { json, ...opts } = init;
	if (json !== undefined) {
		opts.body = JSON.stringify(json);
		opts.headers = { 'content-type': 'application/json', ...opts.headers };
	}
	let res = await fetch(BASE + path, opts);
	if (res.status === 401 && path !== '/users/token') {
		const refreshed = await fetch(`${BASE}/users/token`, { method: 'POST' });
		if (refreshed.ok) res = await fetch(BASE + path, opts);
	}
	if (!res.ok) {
		const body = await res.json().catch(() => ({}));
		throw new ApiError(res.status, body.error ?? res.statusText);
	}
	return res.status === 204 ? (undefined as T) : res.json();
}

/** The signed-in user's profile, or null. One request per page; every script shares it. */
let meOnce: Promise<Profile | null> | undefined;
export const me = () => (meOnce ??= api<Profile>('/profiles/me').catch(() => null));

export const esc = (s: unknown) =>
	String(s ?? '').replace(
		/[&<>"']/g,
		(c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!,
	);
export const when = (iso: string) =>
	new Date(iso).toLocaleDateString(undefined, { dateStyle: 'medium' });
/** Markup for an icon rendered by components/Icons.astro; '' if it isn't on the page. */
export const ico = (name: string) => document.getElementById(`ico-${name}`)?.innerHTML ?? '';
export const $ = <T extends HTMLElement = HTMLElement>(sel: string) =>
	document.querySelector(sel) as T;

/** Form submit → JSON call → `then`; errors land in the form's `.error` element. */
export function onSubmit<T>(
	form: HTMLFormElement,
	call: (data: Record<string, string>) => Promise<T>,
	then: (result: T) => void,
) {
	form.onsubmit = async (e) => {
		e.preventDefault();
		const err = form.querySelector('.error')!;
		err.textContent = '';
		const data = Object.fromEntries(new FormData(form)) as Record<string, string>;
		try {
			then(await call(data));
		} catch (ex) {
			err.textContent = (ex as Error).message;
		}
	};
}
