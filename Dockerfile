# syntax=docker/dockerfile:1
FROM rust:1.91.0-bookworm AS builder
RUN apt-get update \
  && apt-get install -y --no-install-recommends cmake libclang-dev clang \
  && rm -rf /var/lib/apt/lists/*

ARG LIGHTHOUSE_REF=unstable
ARG FEATURES=portable
ARG CARGO_BUILD_JOBS
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

WORKDIR /src
ADD --keep-git-dir=true https://github.com/sigp/lighthouse.git#${LIGHTHOUSE_REF} lighthouse
COPY scripts/build.sh scripts/build.sh
COPY patches patches
COPY timeskip timeskip
RUN scripts/build.sh --lighthouse-dir lighthouse --prepare-only

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/lighthouse/target,sharing=locked \
    cd lighthouse \
    && cargo build --release -p timeskip --features "$FEATURES" ${CARGO_BUILD_JOBS:+--jobs "$CARGO_BUILD_JOBS"} \
    && cp target/release/timeskip /usr/local/bin/timeskip

FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/ethpandaops/timeskip"
# python3 runs ethereum-package's chain insertion scripts inside this image.
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates python3 \
  && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/timeskip /usr/local/bin/timeskip
ENTRYPOINT ["timeskip"]
