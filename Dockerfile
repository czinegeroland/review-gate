FROM rust:1-slim-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock README.md ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git curl jq \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/review-gate /usr/local/bin/review-gate
COPY entrypoint.sh /usr/local/bin/review-gate-entrypoint
RUN chmod +x /usr/local/bin/review-gate-entrypoint

# Run against a mounted repository; the action overrides this entrypoint.
WORKDIR /src
ENTRYPOINT ["/usr/local/bin/review-gate"]
CMD ["--help"]
