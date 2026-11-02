#!/usr/bin/env bash
# Runs once when the micro cluster is first initialised. POSTGRES_DB (user_api) already exists
# and is pg_cron's database; the other services get their own databases here. The
# session_verifier role is created by user-api's migration too, without a password; the dev
# password is set here so the other services can connect to it.
set -euo pipefail

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-SQL
    CREATE DATABASE profile_api;
    CREATE DATABASE article_api;
    CREATE DATABASE comment_api;
    CREATE ROLE session_verifier LOGIN PASSWORD 'verifier';
SQL
