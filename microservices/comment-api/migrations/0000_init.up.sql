CREATE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- article_id and user_id are ids owned by article-api and user-api; no FKs across services.
CREATE TABLE comments (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  article_id  uuid NOT NULL,
  user_id     uuid NOT NULL,
  content     text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER set_updated_at
BEFORE UPDATE ON comments
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

-- (article_id, id) serves the keyset page: WHERE article_id = $1 AND id > $2 ORDER BY id.
CREATE INDEX idx_comments_article_id ON comments (article_id, id);
CREATE INDEX idx_comments_user_id ON comments (user_id);
