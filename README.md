## Features

- Publish articles
- Follow people
- Load your feed
- Favorite articles
- Leave comments
- Manage your profile

## API

All endpoints are under the `/api/v1` path.

### Users

- `POST /users/register` - Register a new user
- `POST /users/login` - Login a new user
- `POST /users/token` - Refresh a user session token
- `GET /users/me` - The signed-in user's id

### Profiles

- `GET /profiles/:uuid` - Get a user's profile (`404` if unknown)
  - A profile is `{"id", "name", "pronouns", "bio", "followers_count", "following"}`.
    `following` is whether the signed-in viewer follows it; anonymous viewers get `false`.
- `GET /profiles/me` - The signed-in user's profile
- `PUT /profiles/me` - Replace the signed-in user's profile
  - Takes `{"name", "pronouns"?, "bio"?}` and replaces all three (omitted
    optionals become null). Limits: name 1-100 chars, pronouns 50, bio 2000; violations are `422`.
- `POST /profiles/:id/follow` - Follow a profile. Following yourself is `422`, an unknown id `404`.
- `DELETE /profiles/:id/follow` - Unfollow a profile (unknown id `404`)
  - Both are idempotent and return only `{"followers_count"}` for that profile.

### Articles

- `GET /articles/feed` - Get a feed of articles, sorted by most recently posted
  - If you're logged in and follow anyone, you only get the articles from those you follow
    (your own excluded). Anonymous viewers and users who follow nobody get every article.
  - Following someone backfills their latest 100 articles into your feed; newer ones arrive as
    they are published (fan-out on write).
  - Query: `?size=` (default 20, max 100) and `?cursor=` (the `next_cursor` of the previous
    page). A malformed cursor or size is `400`.
  - Returns `{"articles": [...], "pagination": {"next_cursor", "has_more", "size"}}`.
    `next_cursor` is the id of the last article on the page (the nil UUID when the page is
    empty); keep fetching while `has_more` is true.
  - A feed article is `{"id", "creator": {"id", "name", "pronouns"}, "title", "subtitle",
"created_at", "updated_at", "tags", "favorites", "favorited"}`. Timestamps are RFC 3339;
    `favorited` is whether the signed-in viewer favorited it.
- `GET /articles/:id` - Get a specific article (`404` if unknown)
  - Same shape as a feed article plus `"content"`.
- `POST /articles/new` - Create an article (`201`, returns the article)
  - Takes `{"title", "subtitle"?, "content", "tags"?}`. Limits: title 1-200 chars, subtitle
    300, content 50000, at most 10 tags of 1-30 chars (trimmed, lowercased, deduplicated);
    violations are `422`. Bodies over 256 KiB are `413`.
- `PUT /articles/:id` - Replace an article you created (same body and limits; `tags` replaces
  the whole set). An unknown id or someone else's article is `404`.
- `POST /articles/:id/favorite` - Like an article
- `DELETE /articles/:id/favorite` - Unlike an article
  - Both are idempotent and return only `{"favorites"}`; an unknown id is `404`.

### Comments

- `GET /comments/:article_id` - Get comments on an article, oldest first (public; `404` if the
  article is unknown)
  - Query: `?size=` (default 20, max 100) and `?cursor=` (the `next_cursor` of the previous
    page). A malformed cursor or size is `400`.
  - Returns `{"comments": [...], "pagination": {"next_cursor", "has_more", "size"}}`, paginated
    like the article feed. A comment is `{"id", "article_id", "author": {"id", "name",
    "pronouns"}, "content", "created_at", "updated_at"}`.
- `POST /comments/:article_id` - Comment on an article (`201`, returns the comment)
  - Takes `{"content"}`, 1-2000 chars after trimming (`422`). Unknown article is `404`.
- `PUT /comments/:article_id/:comment_id` - Update a comment (users can only update their own
  comments). Same body; anything you may not edit is `404`.
- `DELETE /comments/:article_id/:comment_id` - Delete a comment (users can delete their own
  comments and comments on their articles). `204` on success, otherwise `404`.

### Auth model

- `__Host-session` (15 min) is sent on every request; `__Host-refresh` (7 days) is only
  used by `POST /users/token`, which rotates both cookies.
- Expired sessions are deleted by a `pg_cron` job (`sweep-sessions`, every 5 min) installed by
  the migrations. The dev container (`db/Containerfile`, started by `just pg-up`) ships it;
  production Postgres needs `shared_preload_libraries = 'pg_cron'` and `cron.database_name` set
  to the app database. The server checks at startup that the pg_cron launcher is running and the
  job is scheduled, and refuses to start otherwise.
- Replaying a refresh token that has already been rotated revokes the whole session. Clients
  must not call `/users/token` concurrently.
- Login throttling never locks an account. Failed logins are rate-limited per email (5 per
  minute) and per client IP (20 per minute); over the limit, `/users/login` returns `429` with
  `Retry-After` regardless of whether the email exists. Limits live under `[throttle]` in
  `config.toml`; `0` disables one.
- Behind the gateway, set `server.behind_proxy = true` so the IP limit keys on the rightmost
  `X-Forwarded-For` entry instead of the proxy's own address.

## Microservices

Four services (`microservices/{user,profile,article,comment}-api`) share the plumbing in
`microservices/common` (errors, config loading, logging, login throttling, session cookies and
the `AuthUser` extractor). Each service has its own Postgres database and migrations; there are
no cross-service tables, joins or foreign keys.

- Dev cluster: `just pg-micro-up` starts a second Postgres on port 5433 (same pg_cron image) with
  databases `user_api`, `profile_api`, `article_api`, `comment_api` and the `session_verifier`
  role (password `verifier`). pg_cron's database is `user_api`.
- Config: each service reads `config.toml` from its working directory, overridden by
  `<SERVICE>_<SECTION>__<KEY>` env vars (e.g. `USER_API_DB__URL`). `DATABASE_URL` is not read by
  services; it is the compile-time URL for the sqlx macros and the monolith's database.
- Ports: user-api 8201, profile-api 8202, article-api 8203, comment-api 8204.
- Run one: `just run-user-api`.

### Session verification across services

user-api owns `users` and `sessions`. Its migration creates the `session_verifier` role and a
`SECURITY DEFINER` function `verify_session(bytea) -> uuid`; the role can connect to the user
database and execute that function, nothing else. Every other service holds a second pool into
the user database as that role, and `common::auth::AuthUser` verifies the `__Host-session` cookie
with one call to it. Production sets the role's password with `ALTER ROLE session_verifier PASSWORD ...`.

### Internal endpoints

Service-to-service calls use `/internal/...` routes, which the gateway must never expose.

- `POST /internal/profiles` on profile-api, body `{"id": "<uuid>", "name": "..."}` → `201`
  (or `200` if it already exists). Called by user-api during registration; if it fails the new
  user is deleted again and the client gets `502`. Until profile-api exists, point
  `profile_api.url` at a stub that answers `201`.
- `POST /internal/follow-events` on article-api, body
  `[{"id", "follower_id", "followee_id", "kind": "follow" | "unfollow"}, ...]` → `204`.
  profile-api records every follow/unfollow (including cascades) in a `follow_events` outbox
  and a single dispatcher (advisory-locked, so one per cluster) delivers batches of 100 in id
  order, deleting rows only after a `2xx`. Delivery is at-least-once, so article-api must apply
  `follow` as `INSERT ... ON CONFLICT DO NOTHING` and `unfollow` as `DELETE`, in array order.
  Feeds are therefore eventually consistent with the follow graph. While article-api is down,
  events accumulate and profile-api logs a warning with exponential backoff (1 s to 30 s).

### profile-api

Port 8202, database `profile_api`, env prefix `PROFILE_API_`. Serves the Profiles API unchanged,
plus `POST /internal/profiles` for user-api. Needs an `auth_db` pool into `user_api` as
`session_verifier` (8 connections by default). The micro cluster runs with
`max_connections = 300` so four services with two pools each fit.
- `GET /internal/profiles?ids=<uuid>,<uuid>` on profile-api → `[{"id", "name", "pronouns"}]`
  (unknown ids omitted; at most 100 ids). article-api and comment-api call it once per read
  request for the page's distinct authors.

### article-api

Port 8203, database `article_api`, env prefix `ARTICLE_API_`. Serves the Articles API unchanged.
It keeps a replica of the follow graph (`follows`) fed only by `POST /internal/follow-events`
from profile-api's outbox, and runs the same fan-out/backfill triggers as the monolith on top of
it, so feeds are eventually consistent with follows (normally within a second). Author names are
fetched from profile-api on every read; if profile-api is down, non-empty reads are `502`.
- `GET /internal/articles/{id}` on article-api → `{"id", "creator_id"}` or `404`. comment-api
  calls it when creating a comment, when a comment page is empty (to tell "no comments" from
  "no article"), and when someone other than a comment's author tries to delete it (article
  owners may).

### comment-api

Port 8204, database `comment_api`, env prefix `COMMENT_API_`. Serves the Comments API unchanged.
Author names come from profile-api per read; article existence and ownership from article-api
on the paths above. Author edits and author deletes need neither hop.

## Frontend

`frontend/` is a minimal Astro site (static pages, client-side `fetch`) that works against either
stack because both serve the same `/api/v1` contract. `just run-frontend` starts it on
http://localhost:4321 proxying `/api` to the monolith; `API_MICRO=1 pnpm dev` in `frontend/` fans
`/api/v1/{users,profiles,articles,comments}` out to the four services instead. The proxy is not
optional: the API's `__Host-` cookies only flow on the page's own origin, which in production is
the gateway's job. See `frontend/README.md`.
