CREATE TABLE follows (
  follower_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  followee_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (follower_id, followee_id)
);

CREATE INDEX idx_follows_followee_id ON follows (followee_id);

CREATE TABLE profiles (
  user_id         uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  name            text NOT NULL,
  pronouns        text,
  bio             text,
  followers_count bigint NOT NULL DEFAULT 0,  -- maintained by bump_followers_count
  created_at      timestamptz NOT NULL DEFAULT now(),
  updated_at      timestamptz NOT NULL DEFAULT now()
);

-- A follow is not an edit: only bump updated_at when the counter did not move.
CREATE TRIGGER set_updated_at
BEFORE UPDATE ON profiles
FOR EACH ROW
WHEN (OLD.followers_count = NEW.followers_count)
EXECUTE FUNCTION update_updated_at_column();

CREATE INDEX idx_profiles_name ON profiles (name);

-- Row trigger (not application code) so cascade deletes of users keep the count honest.
CREATE FUNCTION bump_followers_count()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE profiles SET followers_count = followers_count + 1 WHERE user_id = NEW.followee_id;
    ELSE
        UPDATE profiles SET followers_count = followers_count - 1 WHERE user_id = OLD.followee_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER bump_followers_count
AFTER INSERT OR DELETE ON follows
FOR EACH ROW
EXECUTE FUNCTION bump_followers_count();
