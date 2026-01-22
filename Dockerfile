# syntax=docker/dockerfile:1

# =============================================================================
# Stage 1: Rust dependency planner (cargo-chef)
# =============================================================================
FROM rust:bookworm AS rust-planner

RUN cargo install cargo-chef

WORKDIR /mudd
COPY mudd/Cargo.toml mudd/Cargo.lock ./
COPY mudd/src ./src
RUN cargo chef prepare --recipe-path recipe.json

# =============================================================================
# Stage 2: Rust dependency cache and build
# =============================================================================
FROM rust:bookworm AS rust-builder

RUN cargo install cargo-chef

WORKDIR /mudd

# Build dependencies (cached layer)
COPY --from=rust-planner /mudd/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY mudd/Cargo.toml mudd/Cargo.lock ./
COPY mudd/src ./src
RUN cargo build --release --bin mudd --bin mudd_init

# =============================================================================
# Stage 3: Node frontend build
# =============================================================================
FROM node:20-bookworm AS node-builder

WORKDIR /client

# Install dependencies (cached layer)
COPY client/package.json client/package-lock.json ./
RUN npm ci

# Build frontend
COPY client/ ./
RUN npm run build

# =============================================================================
# Stage 4: Final runtime image
# =============================================================================
FROM debian:bookworm-slim AS runtime

LABEL org.opencontainers.image.source="https://github.com/jgarzik/mudcroft"
LABEL org.opencontainers.image.licenses="MIT"
LABEL org.opencontainers.image.description="HemiMUD multi-user dungeon server"

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    nginx \
    supervisor \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 -s /bin/bash appuser

# Create directories
RUN mkdir -p /app/lib /data /var/www/html /var/log/supervisor \
    && chown -R appuser:appuser /app /data /var/www/html /var/log/supervisor

# Copy Rust binaries
COPY --from=rust-builder /mudd/target/release/mudd /app/mudd
COPY --from=rust-builder /mudd/target/release/mudd_init /app/mudd_init
RUN chmod +x /app/mudd /app/mudd_init

# Copy Lua library files
COPY --chown=appuser:appuser mudd/lib/*.lua /app/lib/

# Copy frontend static files
COPY --from=node-builder --chown=appuser:appuser /client/dist /var/www/html

# Copy nginx configuration
COPY docker/nginx.conf /etc/nginx/sites-available/default

# Copy supervisord configuration
COPY docker/supervisord.conf /etc/supervisor/conf.d/supervisord.conf

# Copy entrypoint script
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Environment variables with defaults
ENV MUDD_DATABASE_PATH=/data/mudcroft.db \
    MUDD_BIND_ADDR=127.0.0.1:8080 \
    MUDD_LIB_DIR=/app/lib \
    RUST_LOG=mudd=info

# Expose nginx port (mudd runs on localhost only)
EXPOSE 80

# Volume for persistent database
VOLUME ["/data"]

# Health check - verifies both nginx and mudd backend are running
HEALTHCHECK --interval=30s --timeout=3s --retries=3 --start-period=10s \
    CMD curl -f http://localhost/health || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
