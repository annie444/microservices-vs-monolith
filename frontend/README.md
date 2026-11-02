# Frontend

Minimal Astro site over the article API. Static pages, client-side `fetch`, no framework
components. Works unchanged against the monolith or the microservices because both serve the same
`/api/v1` contract.

```sh
pnpm install
pnpm dev                 # http://localhost:4321, proxies /api → monolith on 8181
API_MICRO=1 pnpm dev     # proxies /api/v1/{users,profiles,articles,comments} → ports 8201–8204
API_URL=http://host:port pnpm dev   # any other single backend
```

The API's `__Host-` cookies only flow on the page's own origin, so `/api` must be proxied, never
called cross-origin. `pnpm build` emits `dist/`; whatever serves it in production has to proxy
`/api` the same way (the gateway's job). `astro preview` does not apply the proxy.

Pages: `/` feed · `/article?id=` article, favorite, comments · `/profile?id=` (or `/profile` for
yours, with an edit form) follow/unfollow · `/write` (or `/write?id=` to edit) · `/login` with
registration. Logged-in state comes from `GET /profiles/me` once per page load.

Not here on purpose: logout (the API has no endpoint), comment editing, markdown rendering.
