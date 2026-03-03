# Stage 1: Build
FROM rust:1.93-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/
COPY examples/ examples/

RUN cargo build --release && cargo build --release --examples

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

# Create non-root user with UID 1000 to match typical host user.
# This is required for bind-mounted volumes (./logs) to be writable.
RUN groupadd -g 1000 botuser && useradd -u 1000 -g 1000 -s /bin/false botuser

WORKDIR /app
COPY --from=builder /app/target/release/polymarket-bot .
COPY --from=builder /app/target/release/examples/ ./examples/

RUN mkdir -p logs && chown botuser:botuser logs

USER botuser

CMD ["./polymarket-bot"]
