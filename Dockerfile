FROM rust:1-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && printf 'fn main() {}' > src/main.rs && cargo build --release
COPY src ./src
COPY config ./config
COPY assets ./assets
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libglib2.0-0 libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/chronos-engine /usr/local/bin/chronos-engine
ENV MEDIA_ROOT=/media BIND_ADDR=0.0.0.0:8080 RUST_LOG=info
EXPOSE 8080/tcp 10000-10049/udp
ENTRYPOINT ["/usr/local/bin/chronos-engine"]
