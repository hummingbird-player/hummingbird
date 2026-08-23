#!/usr/bin/env bash
set -euo pipefail

api="https://git.mailliw.org/api/v1/repos/hummingbird/hummingbird"
tag="${RELEASE_TAG:-latest}"

if [[ "$#" -eq 0 ]]; then
  echo "Usage: $0 <asset> [asset...]" >&2
  exit 1
fi

assets=("$@")

if [[ -z "${FORGEJO_TOKEN:-}" ]]; then
  echo "FORGEJO_TOKEN is required" >&2
  exit 1
fi

for asset in "${assets[@]}"; do
  if [[ ! -f "$asset" ]]; then
    echo "Missing release asset: $asset" >&2
    exit 1
  fi
done

auth_header="Authorization: token $FORGEJO_TOKEN"
release_json="$(curl --fail-with-body --silent --show-error --header "$auth_header" "$api/releases/tags/$tag")"
release_id="$(jq -r '.id' <<< "$release_json")"

if [[ -z "$release_id" || "$release_id" == "null" ]]; then
  echo "Release '$tag' does not exist" >&2
  exit 1
fi

current_assets="$(curl --fail-with-body --silent --show-error --header "$auth_header" "$api/releases/$release_id/assets")"

for asset in "${assets[@]}"; do
  name="$(basename "$asset")"

  while IFS= read -r asset_id; do
    [[ -n "$asset_id" && "$asset_id" != "null" ]] || continue
    curl --fail-with-body --silent --show-error \
      --request DELETE \
      --header "$auth_header" \
      "$api/releases/$release_id/assets/$asset_id" \
      >/dev/null
  done < <(jq -r --arg name "$name" '.[] | select(.name == $name) | .id' <<< "$current_assets")
done

for asset in "${assets[@]}"; do
  name="$(basename "$asset")"

  curl --fail-with-body --silent --show-error \
    --request POST \
    --header "$auth_header" \
    --form "attachment=@$asset" \
    "$api/releases/$release_id/assets?name=$name" \
    >/dev/null
  echo "Uploaded $name"
done
