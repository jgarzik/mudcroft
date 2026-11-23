# syntax=docker/dockerfile:1

# Base images are pinned by digest; Dependabot proposes updates.

# =============================================================================
# Stage 1: Rust dependency planner (cargo-chef)
# =============================================================================
FROM rust:1-trixie@sha256:223cc32ddca8f3ee90c436dbc94b6b5f052ea7f9653c5201e9612f08ad7d21b8 AS rust-planner

RUN cargo install cargo-chef --locked --version 0.1.78

WORKDIR /mudd
COPY mudd/Cargo.toml mudd/Cargo.lock ./
COPY mudd/src ./src
RUN cargo chef prepare --recipe-path recipe.json

# =============================================================================
# Stage 2: Rust dependency cache and build
# =============================================================================
FROM rust:1-trixie@sha256:223cc32ddca8f3ee90c436dbc94b6b5f052ea7f9653c5201e9612f08ad7d21b8 AS rust-builder

RUN cargo install cargo-chef --locked --version 0.1.78

WORKDIR /mudd

# Build dependencies (cached layer)
COPY --from=rust-planner /mudd/recipe.json recipe.json
RUN cargo chef cook --locked --release --recipe-path recipe.json

# Build application
COPY mudd/Cargo.toml mudd/Cargo.lock ./
COPY mudd/src ./src
RUN cargo build --locked --release --bin mudd --bin mudd_init --bin mudd_admin

# =============================================================================
# Stage 3: Node frontend build
# =============================================================================
FROM node:24-trixie-slim@sha256:af02d57b57719b9657a45199a79264a9437940cd55b3c7b3fc99615654f124a0 AS node-builder

WORKDIR /client

# Install dependencies (cached layer)
COPY client/package.json client/package-lock.json ./
RUN npm ci --ignore-scripts

# Build frontend
COPY client/ ./
RUN npm run build

# =============================================================================
# Stage 4: Final runtime image
# =============================================================================
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS runtime

LABEL org.opencontainers.image.source="https://github.com/jgarzik/mudcroft"
LABEL org.opencontainers.image.licenses="MIT"
LABEL org.opencontainers.image.description="Mudcroft multi-user dungeon server"

# mudd serves the client, the API and the WebSocket itself (gzip and
# security headers included): no proxy or process supervisor needed.
# curl is for the health check; setpriv (util-linux) drops root.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Unprivileged user that runs mudd_init and mudd
RUN useradd -m -u 1000 -s /usr/sbin/nologin appuser \
    && mkdir -p /app/lib /app/scripts /app/static /data \
    && chown appuser:appuser /data

COPY --from=rust-builder /mudd/target/release/mudd /app/mudd
COPY --from=rust-builder /mudd/target/release/mudd_init /app/mudd_init
COPY --from=rust-builder /mudd/target/release/mudd_admin /app/mudd_admin
COPY mudd/lib/*.lua /app/lib/
# Scripts for POST /universe/{id}/run_script (read relative to /app)
COPY scripts/*.lua /app/scripts/
COPY --from=node-builder /client/dist /app/static
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

WORKDIR /app

ENV MUDD_DATABASE_PATH=/data/mudcroft.db \
    MUDD_BIND_ADDR=0.0.0.0:8080 \
    MUDD_LIB_DIR=/app/lib \
    RUST_LOG=mudd=info

EXPOSE 8080

VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=3s --retries=3 --start-period=15s \
    CMD curl -fsS http://localhost:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
