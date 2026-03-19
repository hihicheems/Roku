#!/usr/bin/env bash

set -euo pipefail

# Development-only lifecycle wrapper for the external OpenViking provider.
#
# Why this script exists:
# - Roku's live memory/runtime paths can use OpenViking as the external memory
#   provider.
# - The product/runtime config should describe *how to connect* to OpenViking,
#   but it should not own local-dev process management details such as PID files
#   and log paths.
# - For local development we still want one stable, repo-local place to start,
#   stop, inspect, and smoke-test the provider.
#
# Repo-local storage choice (Scheme A):
# - PID and log pointer files live under `run/dev-services/`
# - background stdout/stderr logs live under `logs/dev-services/`
# - all of these files should be gitignored
#
# How OpenViking is launched:
# - `run` is the primitive foreground command
# - `start` backgrounds that same `run` command and health-gates it
# - the actual server runner is resolved in this order:
#   1. `OPENVIKING_SERVER_CMD` if the user wants to provide an explicit command
#   2. `OPENVIKING_VENV_DIR/bin/openviking-server` if a repo-local venv exists
#   3. `openviking-server` from the current `PATH`
#   4. a source checkout launched via
#      `python -m openviking_cli.server_bootstrap`
#      with `PYTHONPATH=$OPENVIKING_SOURCE_DIR`
#
# So the answer to "is this source startup or python startup?" is:
# - it depends on what is available locally
# - that resolution is centralized here instead of being duplicated in `just`
# - the chosen runner kind is printed before the server process is `exec`'d

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_BIN="${OPENVIKING_PYTHON:-/opt/homebrew/bin/python3.13}"
OPENVIKING_SOURCE_DIR="${OPENVIKING_SOURCE_DIR:-/tmp/OpenViking-src}"
OPENVIKING_VENV_DIR="${OPENVIKING_VENV_DIR:-$ROOT_DIR/.roku/openviking/venv}"
OPENVIKING_HOST="${ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__HOST:-127.0.0.1}"
OPENVIKING_PORT="${ROKU_RUNTIME__MEMORY__OPENVIKING__PROCESS__SERVER__PORT:-1933}"
OPENVIKING_RUN_DIR="${OPENVIKING_RUN_DIR:-$ROOT_DIR/run/dev-services}"
OPENVIKING_LOG_DIR="${OPENVIKING_LOG_DIR:-$ROOT_DIR/logs/dev-services}"
OPENVIKING_PID_FILE="${OPENVIKING_PID_FILE:-$OPENVIKING_RUN_DIR/openviking.pid}"
OPENVIKING_LOG_POINTER_FILE="${OPENVIKING_LOG_POINTER_FILE:-$OPENVIKING_RUN_DIR/openviking.log_path}"
OPENVIKING_STARTUP_TIMEOUT_SECONDS="${OPENVIKING_STARTUP_TIMEOUT_SECONDS:-60}"

load_env_file() {
	if [[ -f "$ROOT_DIR/.env" ]]; then
		set -a
		# shellcheck disable=SC1091
		. "$ROOT_DIR/.env"
		set +a
	fi
}

load_env_file

ensure_dev_layout() {
	mkdir -p "$OPENVIKING_RUN_DIR" "$OPENVIKING_LOG_DIR"
}

timestamp_for_filename() {
	date -u +"%Y%m%dT%H%M%SZ"
}

next_log_file() {
	printf '%s/openviking-%s.stdout.log\n' \
		"$OPENVIKING_LOG_DIR" \
		"$(timestamp_for_filename)"
}

service_pid() {
	if [[ ! -f "$OPENVIKING_PID_FILE" ]]; then
		return 1
	fi

	local pid
	pid="$(tr -d '[:space:]' < "$OPENVIKING_PID_FILE")"
	if [[ -z "$pid" ]]; then
		return 1
	fi

	printf '%s\n' "$pid"
}

service_running() {
	local pid
	pid="$(service_pid)" || return 1
	kill -0 "$pid" 2>/dev/null
}

service_log_path() {
	if [[ -f "$OPENVIKING_LOG_POINTER_FILE" ]]; then
		cat "$OPENVIKING_LOG_POINTER_FILE"
	fi
}

service_health_running() {
	health_check >/dev/null 2>&1
}

kill_process_tree() {
	local root_pid="$1"
	local child_pids
	child_pids="$(pgrep -P "$root_pid" 2>/dev/null || true)"
	local child_pid
	for child_pid in $child_pids; do
		kill_process_tree "$child_pid"
	done
	kill "$root_pid" 2>/dev/null || true
}

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
	local config_json
	config_json="$(prepare_config_json)"
	printf '%s' "$config_json" | "$PYTHON_BIN" -c 'import json,sys; data=json.load(sys.stdin); print(data.get("generated_openviking_config_path") or "")'
}

wait_timeout_seconds() {
	if [[ -n "${OPENVIKING_WAIT_TIMEOUT_SECONDS:-}" ]]; then
		printf '%s\n' "$OPENVIKING_WAIT_TIMEOUT_SECONDS"
		return 0
	fi

	local config_json
	config_json="$(prepare_config_json)"
	printf '%s' "$config_json" | "$PYTHON_BIN" -c '
import json, math, sys
data = json.load(sys.stdin)
adapter = ((data.get("openviking") or {}).get("adapter") or {})
timeout_ms = int(adapter.get("write_wait_timeout_ms") or 120000)
print(max(1, math.ceil(timeout_ms / 1000)))
'
}

OPENVIKING_RUNNER_KIND=''
OPENVIKING_RUNNER_COMMAND=''

resolve_server_runner() {
	if [[ -n "${OPENVIKING_SERVER_CMD:-}" ]]; then
		OPENVIKING_RUNNER_KIND='explicit OPENVIKING_SERVER_CMD'
		OPENVIKING_RUNNER_COMMAND="$OPENVIKING_SERVER_CMD"
		return 0
	fi

	if [[ -x "$OPENVIKING_VENV_DIR/bin/openviking-server" ]]; then
		OPENVIKING_RUNNER_KIND='venv-installed openviking-server'
		OPENVIKING_RUNNER_COMMAND="$(printf '%q' "$OPENVIKING_VENV_DIR/bin/openviking-server")"
		return 0
	fi

	if command -v openviking-server >/dev/null 2>&1; then
		OPENVIKING_RUNNER_KIND='PATH-installed openviking-server'
		OPENVIKING_RUNNER_COMMAND='openviking-server'
		return 0
	fi

	if [[ -d "$OPENVIKING_SOURCE_DIR" ]] && [[ -x "$PYTHON_BIN" ]]; then
		OPENVIKING_RUNNER_KIND='source checkout via python -m openviking_cli.server_bootstrap'
		OPENVIKING_RUNNER_COMMAND="$(
			printf 'PYTHONPATH=%q %q -m openviking_cli.server_bootstrap' \
				"$OPENVIKING_SOURCE_DIR" "$PYTHON_BIN"
		)"
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

	resolve_server_runner
	cd "$ROOT_DIR"
	echo "Launching OpenViking runner: $OPENVIKING_RUNNER_KIND" >&2
	echo "Config path: $config_path" >&2
	eval "$OPENVIKING_RUNNER_COMMAND --config \"$config_path\" --host \"$OPENVIKING_HOST\" --port \"$OPENVIKING_PORT\""
}

health_check() {
	curl -sf "$(server_health_url)"
}

wait_for_health() {
	local pid="$1"
	local startup_timeout="$OPENVIKING_STARTUP_TIMEOUT_SECONDS"
	local elapsed=0

	while [[ "$elapsed" -lt "$startup_timeout" ]]; do
		if health_check >/dev/null 2>&1; then
			return 0
		fi
		if ! kill -0 "$pid" 2>/dev/null; then
			return 1
		fi
		sleep 1
		elapsed="$((elapsed + 1))"
	done

	return 1
}

start_server() {
	ensure_dev_layout

	if service_running; then
		local pid
		pid="$(service_pid)"
		echo "OpenViking is already running (pid=$pid)"
		return 0
	fi

	if service_health_running; then
		echo "OpenViking is already healthy at $(server_health_url), but it is not managed by this script. Refusing to start a duplicate server." >&2
		return 1
	fi

	rm -f "$OPENVIKING_PID_FILE"

	local log_file
	log_file="$(next_log_file)"
	(
		cd "$ROOT_DIR"
		nohup ./scripts/dev-openviking.sh run >>"$log_file" 2>&1 &
		local pid=$!
		printf '%s\n' "$pid" >"$OPENVIKING_PID_FILE"
		printf '%s\n' "$log_file" >"$OPENVIKING_LOG_POINTER_FILE"
		echo "OpenViking started (pid=$pid, log=$log_file)"
	)

	local pid
	pid="$(service_pid)"
	if wait_for_health "$pid"; then
		echo "OpenViking is healthy"
		return 0
	fi

	echo "OpenViking failed to become healthy; see $(service_log_path)" >&2
	stop_server >/dev/null 2>&1 || true
	return 1
}

stop_server() {
	if ! service_running; then
		if service_health_running; then
			echo "OpenViking is running, but not under this script's PID management. Stop the external process manually first." >&2
			return 1
		fi
		rm -f "$OPENVIKING_PID_FILE"
		echo "OpenViking is not running"
		return 0
	fi

	local pid
	pid="$(service_pid)"
	if ! kill -0 "$pid" 2>/dev/null; then
		rm -f "$OPENVIKING_PID_FILE"
		if service_health_running; then
			echo "OpenViking is running, but the managed PID disappeared before stop completed. Another unmanaged process is still serving $(server_health_url)." >&2
			return 1
		fi
		echo "OpenViking is not running"
		return 0
	fi

	kill_process_tree "$pid"

	local attempts=0
	while kill -0 "$pid" 2>/dev/null; do
		attempts="$((attempts + 1))"
		if [[ "$attempts" -ge 40 ]]; then
			kill -9 "$pid" 2>/dev/null || true
			break
		fi
		sleep 0.25
	done

	rm -f "$OPENVIKING_PID_FILE"
	echo "OpenViking stopped"
}

status_server() {
	if service_running; then
		local pid log_path health_status
		pid="$(service_pid)"
		log_path="$(service_log_path)"
		health_status='unhealthy'
		if health_check >/dev/null 2>&1; then
			health_status='healthy'
		fi
		echo "Running (pid=$pid, health=$health_status, log=${log_path:-none})"
		return 0
	fi

	if service_health_running; then
		echo "Running (unmanaged, health=healthy, log=$(service_log_path))"
		return 0
	fi

	if [[ -f "$OPENVIKING_PID_FILE" ]]; then
		echo "Exited (log=$(service_log_path))"
		return 0
	fi

	echo "Stopped"
}

restart_server() {
	stop_server
	start_server
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
  ./scripts/dev-openviking.sh start
  ./scripts/dev-openviking.sh stop
  ./scripts/dev-openviking.sh restart
  ./scripts/dev-openviking.sh status
  ./scripts/dev-openviking.sh health
  ./scripts/dev-openviking.sh wait
  ./scripts/dev-openviking.sh smoke-write
  ./scripts/dev-openviking.sh smoke-search
  ./scripts/dev-openviking.sh smoke-roundtrip

Lifecycle notes:
  - `run` is the foreground primitive. It resolves a runner and `exec`s it.
  - `start` / `stop` / `restart` / `status` are local-development wrappers.
  - Background dev artifacts are repo-local by default:
      run/dev-services/openviking.pid
      run/dev-services/openviking.log_path
      logs/dev-services/openviking-*.stdout.log
  - The actual OpenViking server may be launched from:
      * OPENVIKING_SERVER_CMD
      * a repo-local virtualenv console script
      * a globally installed `openviking-server`
      * a source checkout via Python module bootstrap

Environment:
  OPENVIKING_SERVER_CMD            Explicit server launch command
  OPENVIKING_VENV_DIR              Virtualenv containing openviking-server
  OPENVIKING_SOURCE_DIR            Source checkout used for PYTHONPATH fallback
  OPENVIKING_PYTHON                Python binary used for fallback runner and JSON parsing
  OPENVIKING_WAIT_TIMEOUT_SECONDS  Override for provider wait timeout; defaults to runtime.memory.openviking.adapter.write_wait_timeout_ms
  OPENVIKING_RUN_DIR               Repo-local run-state directory for PID files and pointers
  OPENVIKING_LOG_DIR               Repo-local log directory for background starts
  OPENVIKING_PID_FILE              Explicit PID file override
  OPENVIKING_LOG_POINTER_FILE      Explicit log-pointer file override
  OPENVIKING_STARTUP_TIMEOUT_SECONDS  Max wait time for `start` health gating
EOF
}

case "${1:-}" in
prepare-config)
	prepare_config_json
	;;
run)
	run_server
	;;
start)
	start_server
	;;
stop)
	stop_server
	;;
restart)
	restart_server
	;;
status)
	status_server
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
