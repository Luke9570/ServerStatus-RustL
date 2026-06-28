# syntax=docker/dockerfile:1.7

FROM rust:1-alpine AS builder

WORKDIR /app

RUN apk add --no-cache musl-dev linux-headers zlib-dev git cmake make g++

ENV CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/Cargo.toml
COPY common/build.rs common/build.rs
COPY common/proto common/proto
COPY server/Cargo.toml server/Cargo.toml
COPY server/build.rs server/build.rs
COPY client/Cargo.toml client/Cargo.toml
COPY client/build.rs client/build.rs

RUN mkdir -p common/src server/src client/src \
    && printf '' > common/src/lib.rs \
    && printf 'fn main() {}\n' > server/src/main.rs \
    && printf 'fn main() {}\n' > client/src/main.rs

RUN --mount=type=cache,id=serverstatus-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=serverstatus-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=serverstatus-target,target=/app/target \
    cargo build --release -p stat_server --locked

COPY ./ /app

RUN --mount=type=cache,id=serverstatus-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=serverstatus-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=serverstatus-target,target=/app/target \
    cargo build --release -p stat_server --locked \
    && cp /app/target/release/stat_server /app/stat_server \
    && strip /app/stat_server

FROM scratch AS production
LABEL description="ServerStatus-RustL telemetry panel"

COPY --from=builder /app/stat_server /stat_server

WORKDIR /data
EXPOSE 8080 9394

CMD ["/stat_server", "-c", "/data/config.toml"]
