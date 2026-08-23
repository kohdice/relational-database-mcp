# List available recipes
default:
    @just --list

# Format the source code
fmt:
    cargo fmt

# Check formatting without modifying files
fmt-check:
    cargo fmt --check

# Run Clippy with warnings denied (alias from .cargo/config.toml)
lint:
    cargo lint

# Run the test suite
test:
    cargo test

# Run static checks in explicit order
check: fmt lint

# Run static checks in explicit order for CI
check-ci: fmt-check lint
