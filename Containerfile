# ------------------------------------------------------------------------------
# Stage 0: Install build tools and cargo chef
# ------------------------------------------------------------------------------
FROM docker.io/library/rust:1-slim-trixie AS base
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -y \
 && apt-get install -y build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --version ^0.1 --locked

# ------------------------------------------------------------------------------
# Stage 1: Prepare the dependency recipe
# ------------------------------------------------------------------------------
FROM base AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ------------------------------------------------------------------------------
# Stage 2: Cache & build dependencies, then build the application
# ------------------------------------------------------------------------------
FROM base AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
ARG PACKAGE
RUN cargo build --release --package ${PACKAGE} --bin ${PACKAGE}

# ------------------------------------------------------------------------------
# Stage 3: Minimal Runtime Environment
# ------------------------------------------------------------------------------
FROM docker.io/library/debian:trixie-slim AS runtime
USER 1001
ARG PACKAGE
COPY --from=builder /app/target/release/${PACKAGE} /usr/local/bin/app
WORKDIR /app
CMD ["/usr/local/bin/app"]
