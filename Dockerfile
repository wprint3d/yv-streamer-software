FROM docker.io/library/rust:1.89-bookworm AS build-base

RUN apt-get update && apt-get install -y --no-install-recommends \
        clang \
        libclang-dev \
        pkg-config && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml ./
COPY src ./src

FROM build-base AS test
RUN cargo test

FROM build-base AS builder
RUN cargo build --release

FROM docker.io/library/debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/yv-streamer-software /usr/local/bin/yv-streamer-software

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/yv-streamer-software"]
