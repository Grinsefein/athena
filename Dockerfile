# Multi-stage build for minimal final image
# Using official Rust image which supports ARM64 (for Raspberry Pi)
FROM rust:1.75-slim-bookworm AS builder

WORKDIR /app

# Install dependencies needed for building
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy manifest files first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Build release binary
RUN cargo build --release

# Final runtime image
FROM debian:bookworm-slim

WORKDIR /app

# Install yt-dlp and required dependencies
RUN apt-get update && apt-get install -y \
    curl \
    ffmpeg \
    python3 \
    python3-pip \
    ca-certificates \
    && pip3 install --break-system-packages yt-dlp \
    && rm -rf /var/lib/apt/lists/*

# Create download directory
RUN mkdir -p /tmp/athena-downloads

# Copy built binary from builder
COPY --from=builder /app/target/release/athena /app/athena

# Expose port
EXPOSE 8000

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8000/ || exit 1

# Run the server
CMD ["/app/athena"]
