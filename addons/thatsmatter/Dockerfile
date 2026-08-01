# ThatsMatter HA App / GHCR image
# Build from monorepo root:
#   docker build -f addons/thatsmatter/Dockerfile -t ghcr.io/felipefdl/thatsmatter:0.1.0 .

ARG BUILD_FROM=ghcr.io/home-assistant/base-debian:bookworm

FROM rust:latest AS builder

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    clang \
    libclang-dev \
    libavahi-client-dev \
    pkg-config \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY bridge/ /src/bridge/
WORKDIR /src/bridge
RUN cargo build --release \
  && cp target/release/thatsmatter-bridge /usr/local/bin/thatsmatter-bridge

FROM ${BUILD_FROM}

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    libdbus-1-3 \
    libavahi-client3 \
    libavahi-common3 \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/thatsmatter-bridge /usr/bin/thatsmatter-bridge
COPY addons/thatsmatter/run.sh /run.sh
RUN chmod a+x /run.sh

ARG BUILD_ARCH
ARG BUILD_VERSION=0.1.0
LABEL \
  io.hass.name="ThatsMatter" \
  io.hass.description="Local Matter bridge for Home Assistant entities" \
  io.hass.type="addon" \
  io.hass.arch="${BUILD_ARCH}" \
  io.hass.version="${BUILD_VERSION}" \
  org.opencontainers.image.source="https://github.com/felipefdl/thatsmatter" \
  org.opencontainers.image.licenses="MIT" \
  maintainer="Felipe Lima <felipe@ferreiradelima.com>"

CMD [ "/run.sh" ]
