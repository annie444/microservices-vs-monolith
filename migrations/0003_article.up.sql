CREATE TABLE articles (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  creator_id  uuid NOT NULL REFERENCES users ON DELETE CASCADE,
  title       text NOT NULL,
  subtitle    text,
  content     text NOT NULL,
  favorites_count bigint NOT NULL DEFAULT 0,  -- maintained by bump_favorites_count
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

-- A favorite is not an edit: only bump updated_at when the counter did not move.
CREATE TRIGGER set_updated_at
BEFORE UPDATE ON articles
FOR EACH ROW
WHEN (OLD.favorites_count = NEW.favorites_count)
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
  user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (article_id, user_id)
);

CREATE INDEX idx_article_favorites_user_id ON article_favorites (user_id);
CREATE INDEX idx_article_favorites_article_id ON article_favorites (article_id);

-- Row trigger (not application code) so cascade deletes of users keep the count honest.
CREATE FUNCTION bump_favorites_count()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE articles SET favorites_count = favorites_count + 1 WHERE id = NEW.article_id;
    ELSE
        UPDATE articles SET favorites_count = favorites_count - 1 WHERE id = OLD.article_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER bump_favorites_count
AFTER INSERT OR DELETE ON article_favorites
FOR EACH ROW
EXECUTE FUNCTION bump_favorites_count();
