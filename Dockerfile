# Stage 1: Builder
FROM rust:1.75-slim-bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/gabagool-bot

# Copy manifests first (for better caching)
COPY Cargo.toml Cargo.lock* ./

# Create dummy src to cache dependencies
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy actual source
COPY src/ ./src/

# Build release
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

# Install CA certificates for HTTPS
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary from builder
COPY --from=builder /usr/src/gabagool-bot/target/release/gabagool-bot /app/

# Run as non-root user
RUN useradd -m -u 1000 bot && chown -R bot:bot /app
USER bot

# Use network host at runtime
ENTRYPOINT ["./gabagool-bot"]
