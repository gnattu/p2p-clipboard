#!/bin/bash
set -euo pipefail

# This script is a utility on Apple platforms to run
# p2p-clipboard binaries under the `leaks` CLI tool,
# which can help to diagnose memory leakage in any kind of
# native or runtime-managed code.

example_name="p2p-clipboard"

script_dir=$(dirname $BASH_SOURCE[0])

# Build the example
cargo build

# Sign it with the required entitlements for process debugging.
codesign -s - -v -f --entitlements "$script_dir/debugger.entitlements" "./target/debug/$example_name"

# Run the example binary under `leaks` to look for any leaked objects.
leaks --atExit -- ./target/debug/$example_name