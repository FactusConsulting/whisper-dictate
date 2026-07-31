# Reproducible dev/test environment that mirrors the CI Ubuntu runner so
# "green in the container" == "green in CI", uniformly on Windows (Docker
# Desktop/WSL2), WSL2, native Ubuntu and macOS.
#
# It builds + tests the Linux target (x86_64-unknown-linux-gnu) — that sidesteps
# the Windows MSVC/lib.exe toolchain entirely. The Windows installer is still
# produced by CI on a Windows runner; only running the native Windows GUI build
# locally needs Visual Studio.
#
# Keep the apt list in sync with the "Install Linux Rust UI build deps" step in
# .github/workflows/test.yml (a structure test enforces this).
FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
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
        cmake \
        clang \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/*
# cmake + clang + libclang-dev are extra-vs-CI deps required ONLY by the
# opt-in `whisper-rs-local` cargo feature (whisper.cpp is compiled from
# source via the whisper-rs-sys build script, which also needs libclang
# for bindgen). Default-feature builds don't touch them, so keeping these
# in the container costs disk but no runtime fragility. See the README
# "Local Whisper (experimental)" section.

USER vscode

# rustup only — the exact channel (1.96.0) and components (rustfmt, clippy) are
# pinned by /rust-toolchain.toml and installed on first use / by postCreate.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain none
ENV PATH="/home/vscode/.cargo/bin:${PATH}"
