-- Add up migration script here

CREATE TABLE comments (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  article_id  uuid NOT NULL REFERENCES articles (id) ON DELETE CASCADE,
  user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  content     text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER set_updated_at
BEFORE UPDATE ON comments
FOR EACH ROW
EXECUTE FUNCTION update_updated_at_column();

CREATE INDEX idx_comments_article_id ON comments (article_id, id);
CREATE INDEX idx_comments_user_id ON comments (user_id);
