CREATE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- user_id is the id user-api assigned; there is no users table here (other service).
CREATE TABLE profiles (
  user_id     uuid PRIMARY KEY,
  name        text NOT NULL,
  pronouns    text,
  bio         text,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER set_updated_at
BEFORE UPDATE ON profiles
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

CREATE INDEX idx_profiles_name ON profiles (name);

CREATE TABLE follows (
  follower_id uuid NOT NULL REFERENCES profiles (user_id) ON DELETE CASCADE,
  followee_id uuid NOT NULL REFERENCES profiles (user_id) ON DELETE CASCADE,
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (follower_id, followee_id)
);

CREATE INDEX idx_follows_followee_id ON follows (followee_id);

-- Counters live apart from profiles so a follow rewrites this small row, not the profile
-- (Postgres updates copy the whole row). Rows appear on first follow; reads LEFT JOIN.
CREATE TABLE profile_stats (
  user_id         uuid PRIMARY KEY REFERENCES profiles (user_id) ON DELETE CASCADE,
  followers_count bigint NOT NULL DEFAULT 0
);

-- Row trigger (not application code) so cascade deletes keep the count honest.
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

-- Outbox for article-api, which keeps its own copy of the follow graph for feed fan-out.
-- Written by trigger so it is atomic with the follow row and fires on cascades too. The
-- dispatcher (src/outbox.rs) delivers in id order and deletes rows after a 2xx: at-least-once,
-- so article-api applies events idempotently.
CREATE TABLE follow_events (
  id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  follower_id uuid NOT NULL,
  followee_id uuid NOT NULL,
  kind        text NOT NULL CHECK (kind IN ('follow', 'unfollow')),
  created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION record_follow_event()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO follow_events (follower_id, followee_id, kind)
        VALUES (NEW.follower_id, NEW.followee_id, 'follow');
    ELSE
        INSERT INTO follow_events (follower_id, followee_id, kind)
        VALUES (OLD.follower_id, OLD.followee_id, 'unfollow');
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER record_follow_event
AFTER INSERT OR DELETE ON follows
FOR EACH ROW
EXECUTE FUNCTION record_follow_event();
