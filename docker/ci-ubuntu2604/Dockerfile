# CI image: Ubuntu 26.04 with the full toolchain to build+test whisper-dictate.
#
# Consumed by the `integration-ubuntu-2604` job in .github/workflows/test.yml.
# Published to ghcr.io/factusconsulting/whisper-dictate-ci-ubuntu2604 by
# .github/workflows/build-ci-images.yml on merge to main.
#
# Design goals:
#   - Match the CI Ubuntu runner's apt list (see the "Install Linux Rust UI
#     build deps" step in test.yml) so container == runner behaviour.
#   - Preinstall the whisper-rs-local build deps (cmake/clang/libclang) so
#     the `whisper-rs-local` feature can be built without extra apt work.
#   - Preinstall Vulkan SDK bits (glslc, libvulkan-dev) so a future
#     whisper-rs-vulkan run needs no image bump.
#   - Preinstall ffmpeg + xvfb for synthetic-WAV generation and future
#     headless-UI tests.
#   - Pin the Rust toolchain to the version in /rust-toolchain.toml
#     (1.96.0) so `cargo` on PATH does not race the repo's toolchain file
#     on cold cache.
#
# Keep the image lean: no whisper GGML models baked in (tests download on
# demand into a runner-scoped cache dir). See ./README.md for what NOT to
# add.
ARG CI_BASE_IMAGE=ubuntu:26.04
FROM ${CI_BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# System toolchain + Rust UI build deps (kept in sync with test.yml's
# "Install Linux Rust UI build deps" step) + whisper-rs-local + Vulkan +
# ffmpeg + xvfb.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        build-essential \
        pkg-config \
        cmake \
        clang \
        libclang-dev \
        libssl-dev \
        libdbus-1-dev \
        libwayland-dev \
        libx11-dev \
        libxcb-render0-dev \
        libxcb-shape0-dev \
        libxcb-xfixes0-dev \
        libxkbcommon-dev \
        libxi-dev \
        libxtst-dev \
        libasound2-dev \
        glslc \
        libvulkan-dev \
        ffmpeg \
        xvfb \
        jq \
    && rm -rf /var/lib/apt/lists/*

# Rust toolchain. Version must match /rust-toolchain.toml (1.96.0). rustup
# will still honour the repo's toolchain file when running inside a
# checkout — this preinstall just avoids a cold-cache toolchain download
# on every CI run.
ARG RUST_VERSION=1.96.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}" \
                    --component rustfmt --component clippy \
    && chmod -R a+w "${RUSTUP_HOME}" "${CARGO_HOME}"

# Sanity-fail the build if the layered toolchain drifts from the repo's pin.
RUN rustc --version | grep -q "${RUST_VERSION}" \
    && cargo --version | grep -q "${RUST_VERSION}"

# GitHub Actions mounts the workspace at /__w/<repo>/<repo> and runs as
# root inside the container by default. Leave WORKDIR unset so the
# runner's own $GITHUB_WORKSPACE takes effect.

# Smoke-verifiable metadata for `docker inspect` — bump when the image
# gains a new capability (new apt package, new preinstalled Python dep,
# new Rust component) so consumers can pin.
LABEL org.opencontainers.image.source="https://github.com/FactusConsulting/whisper-dictate" \
      org.opencontainers.image.description="Ubuntu 26.04 CI image for whisper-dictate integration tests" \
      org.opencontainers.image.licenses="MIT"
