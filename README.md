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
- Replaying a refresh token that has already been rotated revokes the whole session. Clients
  must not call `/users/token` concurrently.
- Login throttling never locks an account. Failed logins are rate-limited per email (5 per
  minute) and per client IP (20 per minute); over the limit, `/users/login` returns `429` with
  `Retry-After` regardless of whether the email exists. Limits live under `[throttle]` in
  `config.toml`; `0` disables one.
- Behind the gateway, set `server.behind_proxy = true` so the IP limit keys on the rightmost
  `X-Forwarded-For` entry instead of the proxy's own address.
