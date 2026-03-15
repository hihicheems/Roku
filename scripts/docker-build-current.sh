#!/usr/bin/env bash

set -euo pipefail

# Build a single-platform image that follows the local machine architecture by default.
# You can still override the target platform explicitly when you want to build for a
# different compatible architecture, for example through emulation or a remote builder.

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_IMAGE_TAG="roku:local"

usage() {
	cat <<'EOF'
Usage:
  ./scripts/docker-build-current.sh [image-tag]

Environment:
  DOCKER_BUILD_PLATFORM  Override the detected local platform, for example linux/amd64.
EOF
}

detect_current_platform() {
	local machine
	machine="$(uname -m)"
	case "$machine" in
	x86_64 | amd64)
		printf '%s\n' "linux/amd64"
		;;
	arm64 | aarch64)
		printf '%s\n' "linux/arm64"
		;;
	*)
		echo "unsupported local architecture: $machine" >&2
		exit 1
		;;
	esac
}

main() {
	local image_tag platform
	if [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
		usage
		exit 0
	fi

	image_tag="${1:-$DEFAULT_IMAGE_TAG}"
	# Prefer the host architecture automatically, but allow callers to pin a different
	# Docker platform explicitly when they need a non-default single-arch build.
	platform="${DOCKER_BUILD_PLATFORM:-$(detect_current_platform)}"

	echo "Building Docker image '$image_tag' for local platform '$platform'."
	docker build \
		--platform "$platform" \
		-f "$ROOT_DIR/deploy/Dockerfile" \
		-t "$image_tag" \
		"$ROOT_DIR"
}

main "$@"
