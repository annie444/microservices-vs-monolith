-- Add down migration script here

DROP EXTENSION citext CASCADE;

DROP FUNCTION update_updated_at_column;
