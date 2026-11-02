# Frontend

Minimal Astro site over the article API. Static pages, client-side `fetch`, Tailwind 4 and
astro-icon (Lucide), no framework components. Works unchanged against the monolith or the
microservices because both serve the same `/api/v1` contract.

Styling lives in `src/styles/global.css`: the palette is a set of CSS variables that flip for dark
mode, exposed to Tailwind through `@theme inline` (`bg-paper`, `text-ink`, `text-mute`,
`border-line`, `text-accent`, `text-heart`), plus a few component classes (`btn`, `field`, `meta`,
`error`, `status`). Client-rendered markup cannot use `<Icon>`, so `components/Icons.astro`
renders each needed icon into a `<template>` once and scripts clone it with `ico(name)`.

```sh
pnpm install
pnpm dev                 # http://localhost:4321, proxies /api → monolith on 8181
API_MICRO=1 pnpm dev     # proxies /api/v1/{users,profiles,articles,comments} → ports 8201–8204
API_URL=http://host:port pnpm dev   # any other single backend
```

The API's `__Host-` cookies only flow on the page's own origin, so `/api` must be proxied, never
called cross-origin. `pnpm build` emits `dist/server` (node, standalone) and `dist/client`; whatever fronts it in
production has to proxy `/api` the same way (the gateway's job). `astro preview` does not apply
the proxy. CSP (`security.csp`) is enabled only for production builds: Astro does not support it
under `astro dev`, where the bare header blocks Vite's inline dev styles.

Pages: `/` feed · `/articles/<id>` article, favorite, comments · `/profiles/<id>` follow/unfollow,
`/profiles/me` for yours with an edit form · `/write` and `/articles/<id>/edit` · `/login` with
registration. The three `<id>` routes are `prerender = false`, rendered on demand by the node
adapter (Vite in dev) and 404 for ids that are not UUIDs; the rest are static. Logged-in state
comes from `GET /profiles/me` once per page load.

Not here on purpose: logout (the API has no endpoint), comment editing, markdown rendering.
