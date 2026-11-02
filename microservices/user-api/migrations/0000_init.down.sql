DROP FUNCTION verify_session(bytea);
DROP TABLE sessions;
DROP TABLE users;
DROP FUNCTION update_updated_at_column();
-- session_verifier is a cluster-wide role and is left in place.
