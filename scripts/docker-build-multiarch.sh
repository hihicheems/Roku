#!/usr/bin/env bash

set -euo pipefail

# Build a multi-platform image manifest with docker buildx. When the active builder
# cannot handle multi-platform builds, this script will create a temporary
# docker-container builder automatically. That temporary builder is removed on exit
# by default so local Docker state stays tidy; pass --keep-builder to retain it.

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_IMAGE_TAG="roku:latest"
DEFAULT_PLATFORMS="linux/amd64,linux/arm64"
TEMP_BUILDER=""
KEEP_TEMP_BUILDER="false"

usage() {
	cat <<'EOF'
Usage:
  ./scripts/docker-build-multiarch.sh [image-tag] [--push] [--platforms <list>] [--builder <name>] [--keep-builder]

Defaults:
  image-tag  roku:latest
  platforms  linux/amd64,linux/arm64

Notes:
  - Without --push, this script validates the multi-architecture build with --output=type=cacheonly.
  - If the current buildx driver is not suitable for multi-platform builds, a temporary docker-container
    builder is created automatically and removed on exit by default.
  - Use --keep-builder if you want to reuse that temporary builder for later multi-architecture builds.
EOF
}

require_buildx() {
	if ! docker buildx version >/dev/null 2>&1; then
		echo "docker buildx is required for multi-architecture builds." >&2
		exit 1
	fi
}

current_builder_driver() {
	docker buildx inspect --format '{{ .Driver }}' 2>/dev/null || true
}

current_builder_name() {
	docker buildx inspect --format '{{ .Name }}' 2>/dev/null || true
}

cleanup() {
	if [[ -n "${TEMP_BUILDER:-}" ]] && [[ "${KEEP_TEMP_BUILDER:-false}" != "true" ]]; then
		docker buildx rm "$TEMP_BUILDER" >/dev/null 2>&1 || true
	fi
}

main() {
	local image_tag platforms push builder_name driver output_mode
	local -a build_args

	image_tag="$DEFAULT_IMAGE_TAG"
	platforms="${DOCKER_BUILD_PLATFORMS:-$DEFAULT_PLATFORMS}"
	push="false"
	builder_name="${DOCKER_BUILDX_BUILDER:-}"
	TEMP_BUILDER=""
	KEEP_TEMP_BUILDER="false"

	if [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
		usage
		exit 0
	fi

	if [[ $# -gt 0 ]] && [[ "${1:-}" != --* ]]; then
		image_tag="$1"
		shift
	fi

	while [[ $# -gt 0 ]]; do
		case "$1" in
		--push)
			push="true"
			;;
		--platforms)
			platforms="${2:-}"
			shift
			;;
		--builder)
			builder_name="${2:-}"
			shift
			;;
		--keep-builder)
			KEEP_TEMP_BUILDER="true"
			;;
		--help | -h)
			usage
			exit 0
			;;
		*)
			echo "unknown argument: $1" >&2
			usage >&2
			exit 1
			;;
		esac
		shift
	done

	require_buildx

	if [[ -z "$platforms" ]]; then
		echo "platform list cannot be empty" >&2
		exit 1
	fi

	if [[ -z "$builder_name" ]]; then
		driver="$(current_builder_driver)"
		if [[ -z "$driver" ]] || [[ "$driver" == "docker" ]]; then
			TEMP_BUILDER="roku-multiarch-$$"
			echo "Creating temporary buildx builder '$TEMP_BUILDER' for multi-platform build support."
			docker buildx create --name "$TEMP_BUILDER" --driver docker-container --use >/dev/null
			builder_name="$TEMP_BUILDER"
		else
			builder_name="$(current_builder_name)"
		fi
	fi

	trap cleanup EXIT

	build_args=(
		--platform "$platforms"
		--file "$ROOT_DIR/deploy/Dockerfile"
		-t "$image_tag"
	)

	if [[ -n "$builder_name" ]]; then
		docker buildx inspect --builder "$builder_name" --bootstrap >/dev/null
		build_args=(--builder "$builder_name" "${build_args[@]}")
	fi

	if [[ "$push" == "true" ]]; then
		output_mode="--push"
	else
		output_mode="--output=type=cacheonly"
	fi

	echo "Building Docker image '$image_tag' for platforms '$platforms'."
	docker buildx build "${build_args[@]}" "$output_mode" "$ROOT_DIR"

	if [[ "$push" != "true" ]]; then
		echo "Multi-architecture build validated without push. Re-run with --push to publish the image tag."
	fi

	if [[ -n "$TEMP_BUILDER" ]] && [[ "$KEEP_TEMP_BUILDER" == "true" ]]; then
		echo "Temporary builder '$TEMP_BUILDER' was kept for reuse because --keep-builder was specified."
	fi
}

main "$@"
