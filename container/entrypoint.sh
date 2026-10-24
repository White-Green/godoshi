#!/usr/bin/env bash
set -euo pipefail

endpoint="${GODOSHI_S3_ENDPOINT:-http://s3.internal}"
bootstrap_dir="$(mktemp -d)"
log_path="$(mktemp)"
trap 'rm -rf -- "$bootstrap_dir"' EXIT

# OutboundHandler maps this key to projects/<project_id>/resources/environment.closure_zstd.
echo 'Downloading resources/environment.closure_zstd'
aws --endpoint-url "${endpoint%/}" --no-sign-request \
    --cli-connect-timeout 30 --cli-read-timeout 300 \
    s3 cp s3://resources/environment.closure_zstd \
    "$bootstrap_dir/environment.closure_zstd" --only-show-errors

# The object must be a zstd-compressed `nix-store --export` stream (including dependencies).
# pipefail ensures even a late decompression error prevents setup/build from running.
echo 'Importing environment closure'
zstd -d -c "$bootstrap_dir/environment.closure_zstd" \
    | nix-store --import > "$bootstrap_dir/store-paths"

# Only enter the shell after the entire import succeeds, using its stdout paths.
echo 'Running godoshi-setup and godoshi-build'
build_status=0
nix --extra-experimental-features nix-command \
    shell --offline --stdin \
    --command bash -c 'godoshi-setup && exec godoshi-build' \
    < "$bootstrap_dir/store-paths" > "$log_path" 2>&1 || build_status=$?
if [ "$build_status" -ne 0 ]; then
  echo 'build failed' >&2
fi

aws --endpoint-url "${endpoint%/}" --no-sign-request \
    --cli-connect-timeout 30 --cli-read-timeout 300 \
    s3 cp "$log_path" s3://results/build.log

exit "$build_status"
