#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_BIN="${OPENVIKING_PYTHON:-/opt/homebrew/bin/python3.13}"
OPENVIKING_SOURCE_DIR="${OPENVIKING_SOURCE_DIR:-/tmp/OpenViking-src}"
OPENVIKING_VENV_DIR="${OPENVIKING_VENV_DIR:-$ROOT_DIR/.roku/openviking/venv}"
OPENVIKING_HOST="${ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__HOST:-127.0.0.1}"
OPENVIKING_PORT="${ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__PORT:-1933}"
load_env_file() {
	if [[ -f "$ROOT_DIR/.env" ]]; then
		set -a
		# shellcheck disable=SC1091
		. "$ROOT_DIR/.env"
		set +a
	fi
}

load_env_file

memory_env() {
	export ROKU_RUNTIME__MEMORY__ENABLED="${ROKU_RUNTIME__MEMORY__ENABLED:-true}"
	export ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__MANAGED="${ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__MANAGED:-true}"
	export ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__BASE_URL="${ROKU_RUNTIME__MEMORY__OPENVIKING__CLIENT__BASE_URL:-http://$OPENVIKING_HOST:$OPENVIKING_PORT}"
}

run_roku_memory_command() {
	memory_env
	(
		cd "$ROOT_DIR"
		cargo run -p roku-cmd --features memory-openviking -- memory "$@"
	)
}

prepare_config_json() {
	run_roku_memory_command prepare-config
}

generated_config_path() {
	prepare_config_json | "$PYTHON_BIN" -c 'import json,sys; data=json.load(sys.stdin); print(data.get("generated_openviking_config_path") or "")'
}

wait_timeout_seconds() {
	if [[ -n "${OPENVIKING_WAIT_TIMEOUT_SECONDS:-}" ]]; then
		printf '%s\n' "$OPENVIKING_WAIT_TIMEOUT_SECONDS"
		return 0
	fi

	prepare_config_json | "$PYTHON_BIN" -c '
import json, math, sys
data = json.load(sys.stdin)
adapter = ((data.get("openviking") or {}).get("adapter") or {})
timeout_ms = int(adapter.get("write_wait_timeout_ms") or 120000)
print(max(1, math.ceil(timeout_ms / 1000)))
'
}

resolve_server_runner() {
	if [[ -n "${OPENVIKING_SERVER_CMD:-}" ]]; then
		printf '%s\n' "$OPENVIKING_SERVER_CMD"
		return 0
	fi

	if [[ -x "$OPENVIKING_VENV_DIR/bin/openviking-server" ]]; then
		printf '%q\n' "$OPENVIKING_VENV_DIR/bin/openviking-server"
		return 0
	fi

	if command -v openviking-server >/dev/null 2>&1; then
		printf '%s\n' "openviking-server"
		return 0
	fi

	if [[ -d "$OPENVIKING_SOURCE_DIR" ]] && [[ -x "$PYTHON_BIN" ]]; then
		printf 'PYTHONPATH=%q %q -m openviking_cli.server_bootstrap\n' \
			"$OPENVIKING_SOURCE_DIR" "$PYTHON_BIN"
		return 0
	fi

	echo "failed to resolve OpenViking server runner; set OPENVIKING_SERVER_CMD explicitly" >&2
	exit 1
}

server_health_url() {
	printf 'http://%s:%s/health\n' "$OPENVIKING_HOST" "$OPENVIKING_PORT"
}

run_server() {
	local config_path
	config_path="$(generated_config_path)"
	if [[ -z "$config_path" ]]; then
		echo "managed OpenViking config was not generated; ensure runtime.memory is enabled and process.managed=true" >&2
		exit 1
	fi

	local runner
	runner="$(resolve_server_runner)"
	cd "$ROOT_DIR"
	eval "$runner --config \"$config_path\" --host \"$OPENVIKING_HOST\" --port \"$OPENVIKING_PORT\""
}

health_check() {
	curl -sf "$(server_health_url)"
}

wait_for_processing() {
	local timeout_seconds
	timeout_seconds="$(wait_timeout_seconds)"
	curl -sf \
		-X POST \
		-H 'content-type: application/json' \
		-d "{\"timeout\": ${timeout_seconds}}" \
		"http://$OPENVIKING_HOST:$OPENVIKING_PORT/api/v1/system/wait"
}

smoke_write() {
	run_roku_memory_command write \
		--scope session \
		--session-id phase4-smoke \
		--kind user_preference \
		--summary "Phase 4 smoke preference" \
		"User prefers concise Rust examples."
}

smoke_search() {
	run_roku_memory_command search \
		--scope session \
		--session-id phase4-smoke \
		--limit 3 \
		"concise Rust examples"
}

smoke_roundtrip() {
	smoke_write
	wait_for_processing
	smoke_search
}

usage() {
	cat <<'EOF'
Usage:
  ./scripts/dev-openviking.sh prepare-config
  ./scripts/dev-openviking.sh run
  ./scripts/dev-openviking.sh health
  ./scripts/dev-openviking.sh wait
  ./scripts/dev-openviking.sh smoke-write
  ./scripts/dev-openviking.sh smoke-search
  ./scripts/dev-openviking.sh smoke-roundtrip

Environment:
  OPENVIKING_SERVER_CMD        Explicit server launch command
  OPENVIKING_VENV_DIR          Virtualenv containing openviking-server
  OPENVIKING_SOURCE_DIR        Source checkout used for PYTHONPATH fallback
  OPENVIKING_PYTHON            Python binary used for fallback runner and JSON parsing
  OPENVIKING_WAIT_TIMEOUT_SECONDS  Optional override for the provider wait timeout; defaults to runtime.memory.openviking.adapter.write_wait_timeout_ms
EOF
}

case "${1:-}" in
prepare-config)
	prepare_config_json
	;;
run)
	run_server
	;;
health)
	health_check
	;;
wait)
	wait_for_processing
	;;
smoke-write)
	smoke_write
	;;
smoke-search)
	smoke_search
	;;
smoke-roundtrip)
	smoke_roundtrip
	;;
help|-h|--help|"")
	usage
	;;
*)
	echo "unknown command: ${1:-}" >&2
	usage
	exit 1
	;;
esac
