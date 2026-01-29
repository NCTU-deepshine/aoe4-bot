#!/bin/sh

set -e
cargo fmt --all --
cargo clippy --all-targets --