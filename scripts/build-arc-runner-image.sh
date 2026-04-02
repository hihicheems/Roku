#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

rust_toolchain_version="$(
  awk -F '"' '/^[[:space:]]*channel[[:space:]]*=/ { print $2; exit }' \
    "${repo_root}/rust-toolchain.toml"
)"

if [[ -z "${rust_toolchain_version}" ]]; then
  echo "failed to read Rust channel from rust-toolchain.toml" >&2
  exit 1
fi

if [[ $# -gt 0 ]]; then
  image_ref="$1"
else
  image_repository="${IMAGE_REPOSITORY:-localhost/roku-arc-runner}"
  if git -C "${repo_root}" rev-parse --short=12 HEAD >/dev/null 2>&1; then
    git_short_sha="$(git -C "${repo_root}" rev-parse --short=12 HEAD)"
  else
    git_short_sha="$(date +%Y%m%d%H%M%S)"
  fi
  default_image_tag="rust-${rust_toolchain_version//./-}-${git_short_sha}"
  image_ref="${image_repository}:${default_image_tag}"
fi

build_args=(
  --build-arg "RUST_TOOLCHAIN_VERSION=${rust_toolchain_version}" \
  --build-arg "GIT_CLIFF_VERSION=${GIT_CLIFF_VERSION:-v2.12.0}" \
  --build-arg "CARGO_NEXTEST_VERSION=${CARGO_NEXTEST_VERSION:-cargo-nextest-0.9.132}" \
  --build-arg "HAWKEYE_VERSION=${HAWKEYE_VERSION:-v6.5.1}" \
)

for var in http_proxy https_proxy all_proxy no_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY; do
  if [[ -n "${!var:-}" ]]; then
    build_args+=(--build-arg "${var}=${!var}")
  fi
done

docker_cmd=(docker)
if ! docker info >/dev/null 2>&1; then
  if command -v sudo >/dev/null 2>&1; then
    docker_cmd=(sudo -E docker)
  fi
fi

echo "Building ARC runner image:"
echo "  repo root: ${repo_root}"
echo "  image ref: ${image_ref}"
echo "  rust toolchain: ${rust_toolchain_version}"

"${docker_cmd[@]}" build \
  "${build_args[@]}" \
  --tag "${image_ref}" \
  --file "${repo_root}/deploy/arc-runner/Dockerfile" \
  "${repo_root}"

echo "Built ${image_ref}"
