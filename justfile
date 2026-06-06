# Nyx task runner. Install `just`: `brew install just`.
# Run `just` with no args to list recipes.

# Default: list available recipes.
default:
    @just --list

# Run the app.
run:
    cargo run -p nyx

# Run the nyx-ui component gallery ("storybook").
gallery:
    cargo run -p nyx-ui --example gallery

# Build the whole workspace.
build:
    cargo build --workspace

# Run all tests.
test:
    cargo test --workspace

# Lint with clippy, warnings as errors.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format all code.
fmt:
    cargo fmt --all

# Check formatting without writing.
fmt-check:
    cargo fmt --all --check

# The pre-push gate: format, lint, test.
check: fmt lint test

# Build a macOS .app + .dmg (pass `--universal` for a fat binary).
bundle-mac *ARGS:
    scripts/bundle-mac.sh {{ARGS}}

# Build a Windows .zip (run on Windows; needs PowerShell + the MSVC toolchain).
bundle-windows:
    pwsh scripts/bundle-windows.ps1
