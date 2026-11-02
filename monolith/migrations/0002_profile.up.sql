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
  created_at      timestamptz NOT NULL DEFAULT now(),
  updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER set_updated_at
BEFORE UPDATE ON profiles
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

CREATE INDEX idx_profiles_name ON profiles (name);

-- Counters live apart from profiles so a follow rewrites this small row, not the profile
-- (Postgres updates copy the whole row). Rows appear on first follow; reads LEFT JOIN.
CREATE TABLE profile_stats (
  user_id         uuid PRIMARY KEY REFERENCES profiles (user_id) ON DELETE CASCADE,
  followers_count bigint NOT NULL DEFAULT 0
);

-- Row trigger (not application code) so cascade deletes of users keep the count honest.
CREATE FUNCTION bump_followers_count()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO profile_stats (user_id, followers_count) VALUES (NEW.followee_id, 1)
        ON CONFLICT (user_id) DO UPDATE SET followers_count = profile_stats.followers_count + 1;
    ELSE
        UPDATE profile_stats SET followers_count = followers_count - 1 WHERE user_id = OLD.followee_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER bump_followers_count
AFTER INSERT OR DELETE ON follows
FOR EACH ROW
EXECUTE FUNCTION bump_followers_count();
