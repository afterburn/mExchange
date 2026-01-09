FROM rust:1.85-slim-bookworm AS builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo.toml for dependency caching
COPY trading_bot/Cargo.toml ./Cargo.toml

# Create dummy src to build dependencies
RUN mkdir -p src && echo "fn main() {}" > src/main.rs

# Build dependencies (will be cached)
RUN cargo build --release && rm -rf src

# Copy actual source
COPY trading_bot/src ./src

# Build the actual binary
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/trading_bot /app/trading_bot

ENTRYPOINT ["/app/trading_bot"]
