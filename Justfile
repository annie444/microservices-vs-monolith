prefix := "micro-vs-mono"
pg_user := "postgres"
pg_pass := "postgres"
pg_db := "postgres"

debug := '''
set -euo pipefail
if [[ -v DEBUG && -n "$DEBUG" ]]; then
    set -x
fi
'''

alias b := build
alias l := lint
alias f := fmt
alias t := test

[default]
[private]
default:
    @just --list

lint:
    cargo clippy --workspace -- -D warnings

test:
    cargo nextest run --workspace --all-targets --no-fail-fast

fmt:
    cargo fmt --all

build:
    cargo build --workspace --all-targets

container app:
    podman build -t "ghcr.io/annie444/{{ app }}:latest" \
        --build-arg PACKAGE="{{ app }}" .

pg-up:
    #!/usr/bin/env bash
    {{ debug }}
    if ! podman volume exists "{{ prefix }}-pg-data"; then
        podman volume create "{{ prefix }}-pg-data"
    fi

    if ! podman container exists "{{ prefix }}-pg"; then
        podman build -q -t "{{ prefix }}-pg" db/
        podman run \
            --rm \
            --detach \
            --env "POSTGRES_USER={{ pg_user }}" \
            --env "POSTGRES_PASSWORD={{ pg_pass }}" \
            --env "POSTGRES_DB={{ pg_db }}" \
            --health-cmd "pg_isready -U {{ pg_user }} -d {{ pg_db }}" \
            --health-interval 2s \
            --health-retries 15 \
            --health-timeout 2s \
            --publish 5432:5432 \
            --hostname "{{ prefix }}-pg" \
            --name "{{ prefix }}-pg" \
            --volume "{{ prefix }}-pg-data:/var/lib/postgresql" \
            "{{ prefix }}-pg" \
            -c shared_preload_libraries=pg_cron \
            -c cron.database_name="{{ pg_db }}"
    fi

pg-down: (_down "pg")

pg-clean: (_clean "pg")

pg-logs: (_logs "pg")

_down name:
    #!/usr/bin/env bash
    {{ debug }}
    if podman container exists "{{ prefix }}-{{ name }}"; then
        podman stop "{{ prefix }}-{{ name }}"
    fi

_clean name:
    #!/usr/bin/env bash
    {{ debug }}
    if podman volume exists "{{ prefix }}-{{ name }}-data"; then
        podman volume rm "{{ prefix }}-{{ name }}-data"
    fi

_logs name:
    #!/usr/bin/env bash
    {{ debug }}
    podman logs "{{ prefix }}-{{ name }}"

# Microservices Postgres: one cluster on 5433, one database per service (db/micro-init), pg_cron in user_api.
pg-micro-up:
    #!/usr/bin/env bash
    {{ debug }}
    if ! podman volume exists "{{ prefix }}-pg-micro-data"; then
        podman volume create "{{ prefix }}-pg-micro-data"
    fi

    if ! podman container exists "{{ prefix }}-pg-micro"; then
        podman build -q -t "{{ prefix }}-pg" db/
        podman run \
            --rm \
            --detach \
            --env "POSTGRES_USER={{ pg_user }}" \
            --env "POSTGRES_PASSWORD={{ pg_pass }}" \
            --env "POSTGRES_DB=user_api" \
            --health-cmd "pg_isready -U {{ pg_user }} -d user_api" \
            --health-interval 2s \
            --health-retries 15 \
            --health-timeout 2s \
            --publish 5433:5432 \
            --hostname "{{ prefix }}-pg-micro" \
            --name "{{ prefix }}-pg-micro" \
            --volume "{{ prefix }}-pg-micro-data:/var/lib/postgresql" \
            --volume "$PWD/db/micro-init:/docker-entrypoint-initdb.d:ro" \
            "{{ prefix }}-pg" \
            -c shared_preload_libraries=pg_cron \
            -c cron.database_name=user_api \
            -c max_connections=300
    fi

pg-micro-down: (_down "pg-micro")

pg-micro-clean: (_clean "pg-micro")

pg-micro-logs: (_logs "pg-micro")

run-user-api:
    cd microservices/user-api && cargo run -p user-api

run-profile-api:
    cd microservices/profile-api && cargo run -p profile-api

run-article-api:
    cd microservices/article-api && cargo run

run-comment-api:
    cd microservices/comment-api && cargo run
