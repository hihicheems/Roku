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

Repository-owned ARC assets stop at the runner image and general deployment
guidance. Environment-specific Helm values are intentionally not tracked here.

Workspace installs usually need private details such as:

- corporate proxy endpoints
- internal DNS overrides
- cluster CIDRs
- node IPs
- private image references
- namespace-local secrets

Those values should stay in local deployment artifacts, ignored `outputs/`
files, or a private operations repository instead of this public tree.

## Deployment guidance

When preparing ARC values for a workspace:

- keep the controller and runner scale-set values outside this repository
- keep mutable runner state isolated per pod
- avoid shared writable Cargo, Rustup, and Docker layer directories
- prefer prebuilt runner images plus remote cache reuse in workflows

The live `act` rollout that motivated this layout uses those same rules, but
its concrete values remain intentionally out of tree.
