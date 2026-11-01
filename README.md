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

- `GET /profiles/:uuid` - Get a user's profile
- `GET /profiles/me` - The signed-in user's profile
- `PUT /profiles/me` - Update the signed-in user's profile
- `POST /profiles/follow` - Follow a profile

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
