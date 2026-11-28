FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY crates/ crates/

# Build release binaries
RUN cargo build --release --bin reposync-daemon --bin reposync

# Verify binaries were produced and are executable
RUN test -x /app/target/release/reposync-daemon \
    && test -x /app/target/release/reposync \
    && /app/target/release/reposync --version

# Production UI. The daemon serves static/ next to its executable.
# There is no Node pin in the repo; Node 22 matches the major used to
# build this UI, and bookworm matches the runtime image.
FROM node:22-bookworm AS ui
WORKDIR /src/web-ui
COPY web-ui/package.json web-ui/package-lock.json ./
RUN npm ci
COPY web-ui/ ./
RUN npm run build \
    && test -s dist/index.html \
    && ! grep -R -q 'window.reposyncStatic = "synthetic"' dist

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    subversion \
    git \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -r -s /usr/sbin/nologin -m -d /var/lib/reposync reposync

# Copy binaries
COPY --from=builder /app/target/release/reposync-daemon /usr/local/bin/
COPY --from=builder /app/target/release/reposync /usr/local/bin/
COPY --from=ui /src/web-ui/dist /usr/local/bin/static
RUN chmod -R a+rX /usr/local/bin/static

# Copy default config (read-only for non-root)
COPY config.example.toml /etc/reposync/config.toml
RUN chmod 644 /etc/reposync/config.toml

# Create data directory
RUN mkdir -p /var/lib/reposync && chown reposync:reposync /var/lib/reposync

USER reposync
WORKDIR /var/lib/reposync

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s \
    CMD curl -f http://localhost:8080/api/status/health || exit 1

ENTRYPOINT ["reposync-daemon"]
CMD ["--config", "/etc/reposync/config.toml"]
