# Run all CI checks
ci: fmt-check check clippy test

# Check code formatting
fmt-check:
    cargo fmt --all -- --check

# Check compilation with warnings as errors
check:
    RUSTFLAGS="-D warnings" cargo check --all-targets --all-features

# Run clippy in pedantic mode
clippy:
    cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic

# Run tests
test:
    cargo test --all-features
