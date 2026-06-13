# UI Longevity Policy

## Current UI stack

**Framework:** [egui](https://github.com/emilk/egui) + [eframe](https://github.com/emilk/egui/tree/master/crates/eframe)  
**Active renderer:** glow (OpenGL-based, `default` Cargo feature `ui-egui-glow`)  
**Exit-route renderer:** wgpu (DirectX 12 / Vulkan-based, `ui-egui-wgpu` feature)

## Renderer feature flags

The renderer is chosen at compile time via Cargo features rather than at runtime
so each renderer path has a clean dependency graph and can be built and validated
independently:

| Feature flag     | Renderer | Status                |
|------------------|----------|-----------------------|
| `ui-egui-glow`   | glow     | Default — shipped binary |
| `ui-egui-wgpu`   | wgpu     | Exit route — CI-validated |

A plain `cargo build` (default features) always produces a glow binary.
The wgpu path is selected with `--no-default-features --features ui-egui-wgpu`.

## CI validation: renderer-matrix workflow

`.github/workflows/renderer-matrix.yml` builds **both** renderer variants on
**ubuntu-latest** and **windows-2025** and then runs a headless GUI-launch probe
on Windows so adapter/renderer init regressions are caught before they reach main.
The workflow triggers on:

- PRs touching `src/rust/Cargo.toml`, `src/rust/Cargo.lock`, `src/rust/**`, or the
  workflow file itself — catching every eframe/egui dependency bump.
- Weekly (Monday 06:00 UTC) schedule — catching ecosystem rot between PRs.
- Manual `workflow_dispatch`.

The **headless probe** (closes #269) launches the compiled binary's `ui`
subcommand and asserts it survives 15 s without a non-zero exit or panic line.
On `windows-2025` the wgpu renderer falls back to the DX12 WARP software adapter
On `windows-2025` wgpu falls back to the DX12 WARP software adapter and survives
15 s headless (confirmed by CI). Glow exits immediately on the same runner because
it requires an OpenGL context and the runner has no software OpenGL rasteriser
(there is no WARP-equivalent for OpenGL). This is a runner-environment limitation,
not a renderer code bug; the probe soft-skips glow's early exit and only hard-fails
on a panic or explicit adapter-init error message in the output.
Linux is build-only (no display server available on the runner).

## Core/domain isolation invariant

**Core and domain modules must not depend on egui or eframe types.**

The UI is designed as a replaceable facade: `src/rust/ui/` and `src/rust/ui.rs`
are the only files permitted to `use egui`, `use eframe`, or reference `egui::`
/ `eframe::` type paths. `src/rust/main.rs` is explicitly allowed for the
`eframe::run_native` binary entry point.

This invariant is enforced by the Python test
`src/tests/python/test_egui_boundary.py`, which scans every `.rs` file under
`src/rust/` and fails the CI test suite if any file outside the above allowlist
contains egui/eframe references.

## When to change renderer (glow → wgpu)

Switch the **default renderer** from glow to wgpu when any of the following
conditions hold:

1. An eframe release drops or breaks the glow renderer path.
2. The glow renderer requires Windows-specific workarounds (driver quirks, DPI
   issues, or special-casing that wgpu does not need).
3. The renderer-matrix CI consistently shows wgpu green where glow is flaky or
   failing.
4. A real GPU-rendering feature is needed that only the wgpu backend can provide
   (e.g. compute shaders, custom render pipelines).

To switch: flip `default = ["ui-egui-wgpu"]` in `src/rust/Cargo.toml` and update
`sonar.yml`, `test.yml`, and `devcontainer.yml` to pass `--features ui-egui-wgpu`
instead of `--features ui-egui-glow`.

## When to replace the egui/eframe shell

Replace the entire `src/rust/ui/` egui shell (rather than just the renderer)
only when:

- egui or eframe becomes unmaintained and blocks an OS compatibility requirement.
- egui/eframe blocks a required platform feature that cannot be worked around
  within the existing API.

Because core and domain logic are isolated from egui types (enforced by the
boundary test), a shell replacement affects only `src/rust/ui/` and
`src/rust/main.rs` — the dictation engine, settings, and all non-UI modules
remain untouched.
