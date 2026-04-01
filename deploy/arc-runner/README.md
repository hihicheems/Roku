# ARC Runner Deployment

This directory tracks the repository-owned parts of the GitHub Actions Runner
Controller (ARC) rollout for Roku.

The current operating model is:

- Bake the native build toolchain and pinned Rust tooling into a custom ARC
  runner image.
- Keep runner pods ephemeral.
- Keep mutable runner state isolated per pod on `/data`.
- Do not share writable Cargo, Rustup, or Docker layer directories across
  runner pods.
- Use `Swatinem/rust-cache@v2` in workflows for remote dependency reuse instead
  of host-level shared writable caches.

## Why this layout exists

The `act` workspace runner pool previously relied on shared writable hostPath
caches for Cargo and Rustup. Under concurrent ARC scale-out, multiple ephemeral
runner pods could write into the same cache directories at the same time. That
led to lock contention, partial downloads, and flaky build/test behavior.

The repository now treats the runner image as the installation boundary:

- `build-essential`, `pkg-config`, Rust, `cargo-nextest`, `hawkeye`, and
  `git-cliff` are preinstalled in the image.
- Workflows only verify that those binaries already exist.
- Each runner pod only writes to its own `_work`, dind storage, and externals
  directories.

## Files

- `Dockerfile`: custom ARC runner image.
- `controller.values.yaml`: controller chart values template.
- `runner-scale-set.values.yaml`: runner scale set values template.

These values files are templates. Fill in environment-specific proxy domains,
cluster CIDRs, node IPs, and image references before applying them on a
workspace.
