# HoverStare serve 自部署镜像（spec 10）

FROM rust:slim-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p hoverstare

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hoverstare /usr/local/bin/hoverstare
EXPOSE 8080
CMD ["hoverstare", "serve"]
