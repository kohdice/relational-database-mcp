# AGENTS.md

This file provides guidance to AI agents and agentic coding tools when working with code in this repository.

## Project Overview

Relational Database MCP (Model Context Protocol) server implemented in Rust. Currently at the initial skeleton stage with tooling fully configured.

- **Rust Edition**: 2024, MSRV 1.92
- **Toolchain**: Pinned via `rust-toolchain.toml` to Rust 1.92 (includes rustfmt and clippy)
- **License**: MIT

## Development Environment Setup

Uses Nix Flakes + direnv for the development environment.

```bash
# With direnv (recommended)
direnv allow

# Without direnv
nix develop
```

The Nix dev shell provides Rust 1.92 (rust-src, rust-analyzer, rustfmt, clippy) and cargo-deny.

## Build / Test / Lint Commands

Cargo aliases are defined in `.cargo/config.toml`:

```bash
cargo b          # build
cargo c          # check
cargo f          # fmt
cargo l          # clippy
cargo t          # test
cargo r          # run
cargo rr         # run --release
```

### Static Analysis & Quality Checks

```bash
cargo fmt --all -- --check                                  # format check
cargo clippy --all-targets --all-features -- -D warnings    # lint
cargo deny check                                            # license & vulnerability audit
```

## Code Style & Conventions

### rustfmt (`rustfmt.toml`)

- Edition 2024 formatting
- `use_small_heuristics = "Max"` — relaxed width limits
- `reorder_modules = true`

### clippy (`clippy.toml`)

- Lints configured for MSRV 1.92

### cargo-deny (`deny.toml`)

- Allowed licenses: MPL-2.0, MIT, Apache-2.0, BSD-3-Clause, ISC, CC0-1.0, Unicode-3.0
- Wildcard dependencies are denied
- Only the crates.io registry is allowed

## Git Commit Guidelines

Commit messages must use a type prefix as defined in CONTRIBUTING.md:

`feat` / `fix` / `refactor` / `test` / `style` / `chore` / `docs` / `ci` / `perf`
