#!/bin/sh
# Default formats in place; --check verifies instead, for CI.

set -e

if [ "$1" = "--check" ]; then
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    cargo test
else
    cargo fmt --all --
    cargo clippy --all-targets --
fi
