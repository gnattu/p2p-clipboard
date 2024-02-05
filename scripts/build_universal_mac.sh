#!/bin/bash
set -euo pipefail

# This script builds a universal binary under macOS
# Need to have both `arch64-apple-darwin` and `x86_64-apple-darwin` installed as target.

cargo build --target=aarch64-apple-darwin --release
cargo build --target=x86_64-apple-darwin --release
mkdir -p ./target/universal-apple-darwin/release
lipo -create -output ./target/universal-apple-darwin/release/p2p-clipboard ./target/x86_64-apple-darwin/release/p2p-clipboard ./target/aarch64-apple-darwin/release/p2p-clipboard