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

# Run the test suite including the container-backed MySQL/PostgreSQL tests
test-db:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${DOCKER_HOST:-}" ] && command -v podman >/dev/null 2>&1; then
        socket="$(podman machine inspect --format '{{{{.ConnectionInfo.PodmanSocket.Path}}' 2>/dev/null || true)"
        if [ -n "$socket" ]; then
            export DOCKER_HOST="unix://$socket"
        fi
    fi
    cargo test -- --include-ignored

# Run static checks in explicit order
check: fmt lint

# Run static checks in explicit order for CI
check-ci: fmt-check lint
