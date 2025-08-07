# Build stage
FROM rust:1.79-bullseye as builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo files for dependency caching
COPY Cargo.toml ./
COPY crates/ ./crates/

# Create dummy src to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Copy actual source code
COPY src/ ./src/

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -m -u 1000 stealth

# Copy binary
COPY --from=builder /app/target/release/reth-stealth-sidecar /usr/local/bin/
COPY --from=builder /app/target/release/rainbow-attack-tool /usr/local/bin/

# Create directories
RUN mkdir -p /config /data /results && \
    chown -R stealth:stealth /config /data /results

USER stealth

EXPOSE 9000 9090

ENTRYPOINT ["/usr/local/bin/reth-stealth-sidecar"]