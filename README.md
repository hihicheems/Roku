# rust-template

<div align="left">

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white)](https://www.rust-lang.org/)

</div>

A Rust workspace template repository for multi-crate projects.

## Overview

This is a **workspace-based** Rust project template. It provides a clean starting point for Rust applications organized as a Cargo workspace with multiple crates:

- Workspace structure with multiple crates
- Pre-configured CI/CD workflows
- Docker support
- Code formatting and linting setup
- Testing framework (nextest)
- License header checking (hawkeye)

> **Note**: This template is designed for **workspace** projects with multiple crates. If you need a **single-package** template for simple applications, please use [simple-rust-template](https://github.com/itscheems/simple-rust-template) instead.

## Quick Start

### Prerequisites

- Rust (stable toolchain)
- [just](https://github.com/casey/just) - A command runner (optional but recommended)
- [hawkeye](https://github.com/itscheems/hawkeye) - License header checker (required for `just fmt` and `just lint`)
- [cargo-nextest](https://nexte.st/) - Fast test runner (required for `just test`)
- [cloc](https://github.com/AlDanial/cloc) - Lines of code counter (required for `just cloc`, optional)

### Installation

1. Clone this repository:

   ```bash
   git clone https://github.com/your-username/rust-template.git
   cd rust-template
   ```

2. Update `Cargo.toml` with your workspace configuration and crate details

3. Start coding in `crates/cmd/src/main.rs` or add new crates to the workspace

## Development

### Build

```bash
# Debug build
cargo build

# Release build
cargo build --release
```

### Run

```bash
cargo run -p cmd
```

### Using Just Commands

This project uses [just](https://github.com/casey/just) for common tasks:

```bash
# List all available commands
just --list

# Format code
just fmt

# Lint code
just lint
# or use alias
just l

# Run tests
just test
# or use alias
just t

# Calculate lines of code
just cloc
```

### Manual Commands

If you prefer not to use `just`:

```bash
# Format code
cargo fmt --all
hawkeye format

# Lint code
hawkeye check
cargo check --locked --all --all-features --all-targets
cargo clippy --locked --all-targets --workspace -- -D warnings

# Run tests
cargo nextest run --locked --workspace
```

## Project Structure

```
.
├── crates/
│   └── cmd/              # Example command crate
│       ├── src/
│       │   └── main.rs   # Main application entry point
│       └── Cargo.toml    # Crate dependencies and metadata
├── deploy/
│   ├── Dockerfile        # Docker build configuration
│   └── .dockerignore     # Docker ignore patterns
├── .github/
│   └── workflows/        # CI/CD workflows
├── Cargo.toml            # Workspace configuration
├── Cargo.lock            # Dependency lock file
├── justfile              # Just command definitions
├── rust-toolchain.toml   # Rust toolchain version
├── rustfmt.toml          # Rustfmt configuration
├── clippy.toml           # Clippy linting configuration
└── LICENSE               # Apache 2.0 License
```

## Docker

Build and run with Docker:

```bash
cd deploy
docker build -t rust-template .
docker run rust-template
```

## Configuration

- **Rust Edition**: 2024
- **License**: Apache-2.0
- **Rust Toolchain**: Defined in `rust-toolchain.toml`
- **Code Style**: Configured in `rustfmt.toml` and `clippy.toml`
- **Workspace Resolver**: Version 3

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Related Templates

- **Single-Package Template**: [simple-rust-template](https://github.com/itscheems/simple-rust-template) - For single-package applications
