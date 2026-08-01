# Build the ThatsMatter bridge binary and run with rs_matter backend.
# rs-matter-stack needs recent rustc (const arg inference; 1.97+). zeroconf needs avahi + clang.
FROM rust:latest AS build
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    clang \
    libclang-dev \
    libavahi-client-dev \
    pkg-config \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY bridge/Cargo.toml bridge/Cargo.lock ./bridge/
COPY bridge/src ./bridge/src
COPY bridge/rustfmt.toml ./bridge/rustfmt.toml
WORKDIR /src/bridge
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    libdbus-1-3 \
    libavahi-client3 \
    libavahi-common3 \
  && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/bridge/target/release/thatsmatter-bridge /usr/local/bin/thatsmatter-bridge
ENV THATSMATTER_LISTEN=127.0.0.1:18465 \
    THATSMATTER_DATA_DIR=/data \
    THATSMATTER_BRIDGE_NAME=ThatsMatter \
    THATSMATTER_MATTER_BACKEND=rs_matter
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/thatsmatter-bridge"]
