_default:
    @just --list

# Type-check the entire workspace.
check:
    cargo check --workspace --all-targets

# Run the test suite.
test:
    cargo test --workspace --all-targets

# Detect unused dependencies. Must report none.
machete:
    cargo machete

# Measure source-based test coverage.
cov:
    cargo llvm-cov --workspace --summary-only

# Compute the CRAP score.
crap:
    cargo crap

# Run every check expected before opening a PR.
pre-pr: check test machete cov crap

# Format every crate.
fmt:
    cargo fmt --all

# Run clippy with the workspace lint config.
lint:
    cargo clippy --workspace --all-targets -- -D warnings
