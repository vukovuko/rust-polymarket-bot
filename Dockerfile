# Stage 1: Build
FROM rust:1.83-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/

RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false botuser

WORKDIR /app
COPY --from=builder /app/target/release/polymarket-bot .

RUN mkdir logs && chown botuser:botuser logs

USER botuser

CMD ["./polymarket-bot"]
