FROM rust:1.97.1-slim as builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin reflex-worker --bin reflexd

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/reflex-worker /usr/local/bin/
COPY --from=builder /app/target/release/reflexd /usr/local/bin/
CMD ["reflex-worker"]
