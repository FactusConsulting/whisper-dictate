# `ci-ubuntu2604` container image

Ubuntu 26.04 image with the full toolchain to build and test
`whisper-dictate`. Used by the `integration-ubuntu-2604` job in
`.github/workflows/test.yml`.

Registry: `ghcr.io/factusconsulting/whisper-dictate-ci-ubuntu2604`
Tag scheme: `latest` (rebuilt on every merge to `main` that touches this
directory) plus the short SHA of the commit that built it.

## What is in the image

- Ubuntu 26.04 base
- Rust `1.96.0` (matches `/rust-toolchain.toml`) with `rustfmt` + `clippy`
- Python 3.14 (Ubuntu 26.04 default; matches the `unit` job in
  `test.yml`) with `pytest`, `pyyaml`, `numpy`. A `python3.12` alias
  symlinks to `python3.14` so call sites written for 3.12 keep working
  until deadsnakes adds a 26.04 repo.
- Rust UI build deps (kept in sync with the `Install Linux Rust UI build
  deps` step in `test.yml`): `pkg-config`, `libdbus-1-dev`,
  `libwayland-dev`, `libx11-dev`, `libxcb-render0-dev`,
  `libxcb-shape0-dev`, `libxcb-xfixes0-dev`, `libxkbcommon-dev`,
  `libxi-dev`, `libxtst-dev`, `libasound2-dev`
- `whisper-rs-local` build deps: `cmake`, `clang`, `libclang-dev`,
  `libssl-dev`
- Vulkan bits for a future `whisper-rs-vulkan` run: `glslc`,
  `libvulkan-dev`
- `ffmpeg` for synthetic-WAV generation in integration tests
- `xvfb` for future headless-UI runs

## Local build + smoke

```bash
docker build -t whisper-dictate-ci:test docker/ci-ubuntu2604/
docker images | grep whisper-dictate-ci

# Tool presence
docker run --rm whisper-dictate-ci:test \
    bash -c "cargo --version && python3.14 --version && ffmpeg -version | head -1"

# Compile-only sanity (does not run tests, does not need models)
docker run --rm -v "$(pwd)":/workspace -w /workspace whisper-dictate-ci:test \
    bash -c "cargo test --manifest-path src/rust/Cargo.toml -p whisper-dictate-app --no-run"
```

## Publishing (CI)

`.github/workflows/build-ci-images.yml` publishes to GHCR on pushes to
`main` that touch `docker/ci-ubuntu2604/**`. The image is public
(`packages: write` + package visibility set once on GHCR).

## Do NOT add to this image

- Whisper GGML models. They are large (39 MB - 3 GB each) and would
  bloat the image beyond reason. Tests that need a model download it on
  demand into a runner-scoped cache dir.
- `faster-whisper`, `torch`, `nvidia-*` CUDA wheels. The image is
  toolchain-only; runtime STT deps belong in `requirements/cpu.txt` and
  should be installed in a per-job venv.
- Node.js, Java, .NET runtimes. None of the current or planned
  integration jobs need them.
- Any secrets or repo-owner-specific credentials. `GITHUB_TOKEN` in the
  build workflow is the only auth path.

## Bumping

- **New apt package**: add it to the `apt-get install` block, rebuild
  locally, push a commit touching `docker/ci-ubuntu2604/**`. The
  `build-ci-images` workflow rebuilds and pushes `:latest`.
- **Rust toolchain bump**: update `ARG RUST_VERSION=` AND
  `/rust-toolchain.toml` in the same PR — the `RUN rustc --version | grep`
  guard rails will fail the build otherwise.
- **Ubuntu base bump** (e.g. 26.04 -> 28.04): rename the directory to
  `docker/ci-ubuntu2804/`, update the workflow's image name, keep the
  old directory for one release cycle so downstream pipelines can pin.
