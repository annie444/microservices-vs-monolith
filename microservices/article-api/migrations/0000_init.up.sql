CREATE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- creator_id is the user id assigned by user-api; there is no users table here (other service).
CREATE TABLE articles (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  creator_id  uuid NOT NULL,
  title       text NOT NULL,
  subtitle    text,
  content     text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER set_updated_at
BEFORE UPDATE ON articles
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

CREATE INDEX idx_articles_creator_id ON articles (creator_id, id DESC);

CREATE TABLE tags (
  tag         text PRIMARY KEY
);

CREATE TABLE article_tags (
  article_id  uuid NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
  tag         text NOT NULL REFERENCES tags (tag) ON DELETE CASCADE,
  PRIMARY KEY (article_id, tag)
);

CREATE INDEX idx_article_tags_tag ON article_tags (tag);

CREATE TABLE article_favorites (
  article_id  uuid NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
  user_id     uuid NOT NULL,
  PRIMARY KEY (article_id, user_id)
);

CREATE INDEX idx_article_favorites_user_id ON article_favorites (user_id);
CREATE INDEX idx_article_favorites_article_id ON article_favorites (article_id);

-- Counters live apart from articles so a favorite rewrites this small row, not the article
-- (Postgres updates copy the whole row). Rows appear on first favorite; reads LEFT JOIN.
CREATE TABLE article_stats (
  article_id      uuid PRIMARY KEY REFERENCES articles (id) ON DELETE CASCADE,
  favorites_count bigint NOT NULL DEFAULT 0
);

CREATE FUNCTION bump_favorites_count()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO article_stats (article_id, favorites_count) VALUES (NEW.article_id, 1)
        ON CONFLICT (article_id) DO UPDATE SET favorites_count = article_stats.favorites_count + 1;
    ELSE
        UPDATE article_stats SET favorites_count = favorites_count - 1 WHERE article_id = OLD.article_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER bump_favorites_count
AFTER INSERT OR DELETE ON article_favorites
FOR EACH ROW
EXECUTE FUNCTION bump_favorites_count();

-- Replica of profile-api's follow graph, filled only by POST /internal/follow-events (its
-- outbox). Used for feed fan-out; profile-api remains the source of truth.
CREATE TABLE follows (
  follower_id uuid NOT NULL,
  followee_id uuid NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (follower_id, followee_id)
);

CREATE INDEX idx_follows_followee_id ON follows (followee_id);

-- Fan-out on write: each follower's feed is materialized here at publish and follow time, so a
-- feed page is one range scan on the primary key. No index on article_id: it would double the
-- fan-out write cost to speed up a cascade (article deletion) that has no endpoint.
-- ponytail: publish cost grows with the author's follower count; go hybrid (pull for celebrity
-- authors) if publish latency matters.
CREATE TABLE feed_entries (
  user_id    uuid NOT NULL,
  article_id uuid NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, article_id)
);

CREATE FUNCTION fan_out_article()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO feed_entries (user_id, article_id)
    SELECT follower_id, NEW.id FROM follows WHERE followee_id = NEW.creator_id
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER fan_out_article
AFTER INSERT ON articles
FOR EACH ROW
EXECUTE FUNCTION fan_out_article();

-- Following backfills the author's latest 100 articles; unfollowing removes all of theirs.
CREATE FUNCTION sync_feed_on_follow()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO feed_entries (user_id, article_id)
        SELECT NEW.follower_id, id FROM articles
        WHERE creator_id = NEW.followee_id
        ORDER BY id DESC LIMIT 100
        ON CONFLICT DO NOTHING;
    ELSE
        DELETE FROM feed_entries
        WHERE user_id = OLD.follower_id
          AND article_id IN (SELECT id FROM articles WHERE creator_id = OLD.followee_id);
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER sync_feed_on_follow
AFTER INSERT OR DELETE ON follows
FOR EACH ROW
EXECUTE FUNCTION sync_feed_on_follow();
