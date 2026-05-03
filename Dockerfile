# --- Stage 1: Builder ---
FROM --platform=$BUILDPLATFORM rust:1.85-slim-bookworm AS builder
WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Step A: Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
# Create a dummy frontend.html so include_str! doesn't fail during dependency caching
RUN touch frontend.html 
RUN cargo build --release
RUN rm -f target/release/deps/athena*

# Step B: Build actual source
COPY frontend.html .
COPY src ./src
RUN cargo build --release

# --- Stage 2: Runtime ---
FROM debian:bookworm-slim
WORKDIR /app

# Security: Non-root user
RUN groupadd -r athena && useradd -r -g athena athena

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg \
    python3 \
    python3-pip \
    ca-certificates \
    curl \
    && pip3 install --no-cache-dir --break-system-packages yt-dlp \
    && apt-get purge -y --auto-remove \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /tmp/athena-downloads && chown athena:athena /tmp/athena-downloads

# Copy the binary from the builder stage
COPY --from=builder --chown=athena:athena /app/target/release/athena /app/athena

USER athena
EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8000/ || exit 1

CMD ["/app/athena"]
