#!/usr/bin/env bash
set -euo pipefail

image="${1:-hummingbird-ci:rust-1.95-bookworm}"
arm64="${2:-false}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

extra_args=()
if [ "$arm64" = "true" ]; then
  extra_args+=(--build-arg PREWARM_ARM64=true)
fi

docker build \
  "${extra_args[@]}" \
  --file "$repo_root/.woodpecker/images/linux-ci.Dockerfile" \
  --tag "$image" \
  "$repo_root"

docker image inspect "$image" --format 'built {{.RepoTags}} {{.Id}} {{.Size}} bytes'
