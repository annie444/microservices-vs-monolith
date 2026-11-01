CREATE TABLE users (
  id                  uuid PRIMARY KEY DEFAULT uuidv7(),
  email               citext NOT NULL UNIQUE,
  password_hash       text NOT NULL,        -- PHC string: algo + params + salt + hash
  password_changed_at timestamptz NOT NULL DEFAULT now(),
  created_at          timestamptz NOT NULL DEFAULT now(),
  updated_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER set_updated_at
BEFORE UPDATE ON users
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

CREATE TABLE sessions (
  token_hash              bytea PRIMARY KEY,     -- SHA-256 of the session cookie value
  refresh_token_hash      bytea NOT NULL,        -- SHA-256 of the refresh cookie value
  prev_refresh_token_hash bytea,                 -- superseded refresh hash; a hit here is a replay
  user_id                 uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  session_expires_at      timestamptz NOT NULL,  -- short-lived; renewed by /token
  expires_at              timestamptz NOT NULL,  -- absolute refresh-token timeout
  created_at              timestamptz NOT NULL DEFAULT now(),
  last_seen_at            timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX idx_sessions_refresh_token_hash ON sessions (refresh_token_hash);
CREATE INDEX idx_sessions_prev_refresh_token_hash ON sessions (prev_refresh_token_hash);
CREATE INDEX idx_sessions_user_id ON sessions (user_id);
CREATE INDEX idx_sessions_expires_at ON sessions (expires_at);
