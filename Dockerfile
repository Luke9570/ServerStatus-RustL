FROM rust:1-alpine AS builder

WORKDIR /app
COPY ./ /app

RUN apk add --no-cache musl-dev git cmake make g++
RUN cargo build --release -p stat_server --locked
RUN strip /app/target/release/stat_server

FROM scratch AS production
LABEL description="ServerStatus-RustL telemetry panel"

COPY --from=builder /app/target/release/stat_server /stat_server

WORKDIR /data
EXPOSE 8080 9394

CMD ["/stat_server", "-c", "/data/config.toml"]
