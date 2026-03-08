#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR="${ROKU_RUN_DIR:-$ROOT_DIR/run/dev-services}"
LOG_DIR="${ROKU_DEV_SERVICE_LOG_DIR:-$ROOT_DIR/logs/dev-services}"
DEFAULT_API_BIND_ADDR="127.0.0.1:8787"
DEFAULT_OPENROUTER_PRIMARY_MODEL="step-3.5-flash:free"
DEFAULT_OPENROUTER_FALLBACK_MODELS="deepseek-chat,gemini-2.0-flash"

readonly ROOT_DIR RUN_DIR LOG_DIR DEFAULT_API_BIND_ADDR DEFAULT_OPENROUTER_PRIMARY_MODEL DEFAULT_OPENROUTER_FALLBACK_MODELS

load_env_file() {
	if [[ -f "$ROOT_DIR/.env" ]]; then
		set -a
		# shellcheck disable=SC1091
		. "$ROOT_DIR/.env"
		set +a
	fi
}

services() {
	printf '%s\n' "api-gateway" "telegram-bot"
}

embedded_components() {
	printf '%s\n' \
		"roku-runtime-service" \
		"roku-planning-engine" \
		"roku-task-planner" \
		"roku-execution-graph-builder" \
		"roku-agent-runtime" \
		"roku-validation-plane" \
		"roku-state-store" \
		"roku-artifact-store" \
		"roku-observability"
}

service_exists() {
	local target="${1:-}"
	local service
	for service in $(services); do
		if [[ "$service" == "$target" ]]; then
			return 0
		fi
	done
	return 1
}

require_known_service() {
	if ! service_exists "${1:-}"; then
		echo "unknown service: ${1:-}" >&2
		exit 1
	fi
}

service_command() {
	case "${1:-}" in
	api-gateway)
		printf '%s\n' "cargo run -p roku-cmd -- api-gateway"
		;;
	telegram-bot)
		printf '%s\n' "cargo run -p roku-cmd -- telegram-bot"
		;;
	*)
		echo "unknown service: ${1:-}" >&2
		return 1
		;;
	esac
}

service_summary() {
	case "${1:-}" in
	api-gateway)
		printf '%s\n' "Actix HTTP ingress"
		;;
	telegram-bot)
		printf '%s\n' "Telegram long polling"
		;;
	*)
		printf '%s\n' "-"
		;;
	esac
}

service_pid_file() {
	printf '%s/%s.pid\n' "$RUN_DIR" "$1"
}

service_started_at_file() {
	printf '%s/%s.started_at\n' "$RUN_DIR" "$1"
}

service_log_reference_file() {
	printf '%s/%s.log_path\n' "$RUN_DIR" "$1"
}

timestamp_for_filename() {
	date -u +"%Y%m%dT%H%M%SZ"
}

latest_service_log() {
	local service="$1"
	find "$LOG_DIR" -maxdepth 1 -type f -name "${service}-*.stdout.log" 2>/dev/null | sort | tail -n 1
}

service_stdout_log() {
	local service="$1"
	local pointer_file
	pointer_file="$(service_log_reference_file "$service")"
	if [[ -f "$pointer_file" ]]; then
		cat "$pointer_file"
		return 0
	fi

	latest_service_log "$service"
}

service_failure_hint() {
	local service="$1"
	local log_path
	log_path="$(service_stdout_log "$service")"
	if [[ -z "$log_path" ]] || [[ ! -f "$log_path" ]]; then
		return 0
	fi

	awk 'NF { line = $0 } END { if (line) print line }' "$log_path"
}

ensure_layout() {
	mkdir -p "$RUN_DIR" "$LOG_DIR"
}

load_env_prefix() {
	printf '%s' "if [ -f .env ]; then set -a; . ./.env; set +a; fi;"
}

service_pid() {
	local pid_file
	pid_file="$(service_pid_file "$1")"
	if [[ ! -f "$pid_file" ]]; then
		return 1
	fi

	local pid
	pid="$(tr -d '[:space:]' < "$pid_file")"
	if [[ -z "$pid" ]]; then
		return 1
	fi

	printf '%s\n' "$pid"
}

service_running() {
	local pid
	pid="$(service_pid "$1")" || return 1
	kill -0 "$pid" 2>/dev/null
}

service_status() {
	local service="$1"
	local pid_file
	pid_file="$(service_pid_file "$service")"

	if service_running "$service"; then
		printf '%s\n' "Running"
		return 0
	fi

	if [[ -f "$pid_file" ]]; then
		printf '%s\n' "Exited"
		return 0
	fi

	printf '%s\n' "Stopped"
}

elapsed_seconds() {
	local started_at_file
	started_at_file="$(service_started_at_file "$1")"
	if [[ ! -f "$started_at_file" ]]; then
		printf '%s\n' "-"
		return 0
	fi

	local started_at now
	started_at="$(tr -d '[:space:]' < "$started_at_file")"
	now="$(date +%s)"
	if [[ -z "$started_at" ]] || [[ "$started_at" -gt "$now" ]]; then
		printf '%s\n' "-"
		return 0
	fi

	printf '%s\n' "$((now - started_at))"
}

format_duration() {
	local raw_seconds="$1"
	if [[ "$raw_seconds" == "-" ]]; then
		printf '%s\n' "-"
		return 0
	fi

	local seconds days hours minutes remainder
	seconds="$raw_seconds"
	days="$((seconds / 86400))"
	remainder="$((seconds % 86400))"
	hours="$((remainder / 3600))"
	remainder="$((remainder % 3600))"
	minutes="$((remainder / 60))"
	seconds="$((remainder % 60))"

	if [[ "$days" -gt 0 ]]; then
		printf '%s\n' "${days}d${hours}h"
		return 0
	fi

	if [[ "$hours" -gt 0 ]]; then
		printf '%s\n' "${hours}h${minutes}m"
		return 0
	fi

	if [[ "$minutes" -gt 0 ]]; then
		printf '%s\n' "${minutes}m${seconds}s"
		return 0
	fi

	printf '%s\n' "${seconds}s"
}

api_gateway_bind_addr() {
	printf '%s\n' "${ROKU_API_BIND_ADDR:-$DEFAULT_API_BIND_ADDR}"
}

api_gateway_health_url() {
	local bind_addr host port
	bind_addr="$(api_gateway_bind_addr)"
	host="${bind_addr%:*}"
	port="${bind_addr##*:}"
	if [[ -z "$host" ]] || [[ "$host" == "$bind_addr" ]] || [[ "$host" == "0.0.0.0" ]]; then
		host="127.0.0.1"
	fi
	printf 'http://%s:%s/health\n' "$host" "$port"
}

service_endpoint() {
	case "${1:-}" in
	api-gateway)
		api_gateway_health_url
		;;
	*)
		return 1
		;;
	esac
}

http_probe_status() {
	local url="$1"
	if ! command -v curl >/dev/null 2>&1; then
		printf '%s\n' "curl-missing"
		return 0
	fi

	local code
	code="$(curl -m 2 -s -o /dev/null -w '%{http_code}' "$url" || true)"
	if [[ -z "$code" ]] || [[ "$code" == "000" ]]; then
		printf '%s\n' "unreachable"
	else
		printf 'HTTP %s\n' "$code"
	fi
}

status_marker() {
	case "$1" in
	Running|Ready|Configured|Enabled)
		printf '%s' '[ok]'
		;;
	Exited)
		printf '%s' '[!!]'
		;;
	Stopped|Missing|Disabled|Unconfigured)
		printf '%s' '[--]'
		;;
	*)
		printf '%s' '[..]'
		;;
	esac
}

start_service() {
	local service="$1"
	require_known_service "$service"
	ensure_layout

	if service_running "$service"; then
		local running_pid
		running_pid="$(service_pid "$service")"
		echo "service ${service} is already running (pid=${running_pid})"
		return 0
	fi

	local command stdout_log pid_file started_at_file pointer_file timestamp
	command="$(service_command "$service")"
	timestamp="$(timestamp_for_filename)"
	stdout_log="$LOG_DIR/${service}-${timestamp}.stdout.log"
	pid_file="$(service_pid_file "$service")"
	started_at_file="$(service_started_at_file "$service")"
	pointer_file="$(service_log_reference_file "$service")"

	(
		cd "$ROOT_DIR"
		nohup /bin/bash -lc "$(load_env_prefix) cd '$ROOT_DIR'; exec ${command}" \
			>>"$stdout_log" 2>&1 &
		local pid=$!
		printf '%s\n' "$pid" > "$pid_file"
		date +%s > "$started_at_file"
		printf '%s\n' "$stdout_log" > "$pointer_file"
		echo "service ${service} started (pid=${pid}, log=${stdout_log})"
	)
}

stop_service() {
	local service="$1"
	require_known_service "$service"
	local pid_file started_at_file
	pid_file="$(service_pid_file "$service")"
	started_at_file="$(service_started_at_file "$service")"

	if ! service_running "$service"; then
		rm -f "$pid_file" "$started_at_file"
		echo "service ${service} is not running"
		return 0
	fi

	local pid
	pid="$(service_pid "$service")"
	kill "$pid"

	local attempts=0
	while kill -0 "$pid" 2>/dev/null; do
		attempts="$((attempts + 1))"
		if [[ "$attempts" -ge 20 ]]; then
			kill -9 "$pid" 2>/dev/null || true
			break
		fi
		sleep 0.25
	done

	rm -f "$pid_file" "$started_at_file"
	echo "service ${service} stopped"
}

latest_component_log() {
	local component="$1"
	find "$ROOT_DIR/logs/$component" -maxdepth 1 -type f -name '*.log' \
		! -name 'current.log' \
		! -name 'current.log.*' \
		2>/dev/null | sort | tail -n 1
}

service_row() {
	local service="$1"
	local status ready pid uptime detail log_path endpoint probe
	status="$(service_status "$service")"
	log_path="$(service_stdout_log "$service")"
	if [[ -z "$log_path" ]]; then
		log_path='-'
	fi

	if [[ "$status" == "Running" ]]; then
		ready='1/1'
		pid="$(service_pid "$service")"
		uptime="$(format_duration "$(elapsed_seconds "$service")")"
	else
		ready='0/1'
		pid='-'
		uptime='-'
	fi

	detail="$(service_summary "$service")"
	if endpoint="$(service_endpoint "$service" 2>/dev/null)"; then
		probe='not-checked'
		if [[ "$status" == "Running" ]]; then
			probe="$(http_probe_status "$endpoint")"
		fi
		detail="${detail}; ${endpoint} (${probe})"
	fi
	if [[ "$status" == "Exited" ]]; then
		local failure_hint
		failure_hint="$(service_failure_hint "$service")"
		if [[ -n "$failure_hint" ]]; then
			detail="${detail}; last_log=${failure_hint}"
		fi
	fi

	printf '  %-4s %-16s %-5s %-10s %-8s %-8s %-48s %s\n' \
		"$(status_marker "$status")" \
		"$service" \
		"$ready" \
		"$status" \
		"$pid" \
		"$uptime" \
		"$detail" \
		"$log_path"
}

component_detail() {
	case "${1:-}" in
	roku-runtime-service)
		printf '%s\n' 'embedded via api-gateway, telegram-bot'
		;;
	roku-planning-engine)
		printf '%s\n' 'embedded via roku-runtime-service'
		;;
	roku-task-planner)
		printf '%s\n' 'embedded via roku-runtime-service'
		;;
	roku-execution-graph-builder)
		printf '%s\n' 'embedded via roku-runtime-service'
		;;
	roku-agent-runtime)
		printf '%s\n' 'embedded via roku-runtime-service'
		;;
	roku-validation-plane)
		printf '%s\n' 'embedded via roku-runtime-service'
		;;
	roku-state-store)
		if [[ -n "${ROKU_DATABASE_URL:-${DATABASE_URL:-}}" ]]; then
			printf '%s\n' 'postgres configured with in-memory fallback for unset repos'
		else
			printf '%s\n' 'in-memory repositories active by default'
		fi
		;;
	roku-artifact-store)
		printf '%s\n' 'embedded via roku-runtime-service'
		;;
	roku-observability)
		printf '%s\n' 'async rotating file sink + stderr fanout'
		;;
	*)
		printf '%s\n' '-'
		;;
	esac
}

embedded_component_row() {
	local component="$1"
	printf '  %-4s %-30s %-12s %s\n' \
		"$(status_marker Ready)" \
		"$component" \
		"Ready" \
		"$(component_detail "$component")"
}

integration_row() {
	local name="$1"
	local status="$2"
	local detail="$3"
	printf '  %-4s %-20s %-12s %s\n' \
		"$(status_marker "$status")" \
		"$name" \
		"$status" \
		"$detail"
}

log_row() {
	local component="$1"
	local latest_log
	latest_log="$(latest_component_log "$component")"
	if [[ -n "$latest_log" ]]; then
		printf '  %-4s %-24s %-12s %s\n' \
			"$(status_marker Ready)" \
			"$component" \
			"Ready" \
			"$latest_log"
	else
		printf '  %-4s %-24s %-12s %s\n' \
			"$(status_marker Missing)" \
			"$component" \
			"Missing" \
			"no log files yet"
	fi
}

endpoint_row() {
	local name="$1"
	local url="$2"
	local probe="$3"
	local status='Configured'
	if [[ "$probe" == 'unreachable' ]]; then
		status='Stopped'
	fi
	printf '  %-4s %-20s %-12s %s\n' \
		"$(status_marker "$status")" \
		"$name" \
		"$status" \
		"${url} (${probe})"
}

doctor() {
	ensure_layout
	echo 'Roku doctor'
	echo
	printf '%s\n' 'Application Services'
	printf '  %-4s %-16s %-5s %-10s %-8s %-8s %-48s %s\n' 'MARK' 'NAME' 'READY' 'STATUS' 'PID' 'UPTIME' 'DETAILS' 'LOG'
	local service
	for service in $(services); do
		service_row "$service"
	done
		echo
	printf '%s\n' 'Embedded Runtime Components'
	printf '  %-4s %-30s %-12s %s\n' 'MARK' 'NAME' 'STATUS' 'DETAILS'
	local component
	for component in $(embedded_components); do
		embedded_component_row "$component"
	done
		echo
	printf '%s\n' 'Integrations'
	printf '  %-4s %-20s %-12s %s\n' 'MARK' 'NAME' 'STATUS' 'DETAILS'
	local database_url
	database_url="${ROKU_DATABASE_URL:-${DATABASE_URL:-}}"
	if [[ -n "${OPENROUTER_API_KEY:-}" ]]; then
		integration_row 'OpenRouter' 'Configured' "primary=${OPENROUTER_PRIMARY_MODEL:-$DEFAULT_OPENROUTER_PRIMARY_MODEL}; fallback=${OPENROUTER_FALLBACK_MODELS:-$DEFAULT_OPENROUTER_FALLBACK_MODELS}"
	else
		integration_row 'OpenRouter' 'Missing' 'set OPENROUTER_API_KEY in .env or environment'
	fi
	if [[ -n "${TELOXIDE_TOKEN:-${TELEGRAM_BOT_TOKEN:-}}" ]]; then
		integration_row 'Telegram Bot API' 'Configured' 'bot token detected'
	else
		integration_row 'Telegram Bot API' 'Missing' 'set TELOXIDE_TOKEN or TELEGRAM_BOT_TOKEN'
	fi
	if [[ -n "$database_url" ]]; then
		integration_row 'PostgreSQL' 'Configured' 'database URL detected for session memory backend'
	else
		integration_row 'PostgreSQL' 'Unconfigured' 'runtime falls back to in-memory state stores'
	fi
	integration_row 'API bind address' 'Configured' "$(api_gateway_bind_addr)"
		echo
	printf '%s\n' 'Observability Logs'
	printf '  %-4s %-24s %-12s %s\n' 'MARK' 'COMPONENT' 'STATUS' 'LATEST LOG'
	for component in roku-cmd roku-connectors-telegram roku-runtime-service roku-llm-adapter; do
		log_row "$component"
	done
		echo
	printf '%s\n' 'Endpoints'
	printf '  %-4s %-20s %-12s %s\n' 'MARK' 'NAME' 'STATUS' 'DETAILS'
	local health_url probe
	health_url="$(api_gateway_health_url)"
	probe='not-checked'
	if [[ "$(service_status api-gateway)" == 'Running' ]]; then
		probe="$(http_probe_status "$health_url")"
	fi
	endpoint_row 'api-gateway /health' "$health_url" "$probe"
}

usage() {
	cat <<'USAGE'
Usage:
  scripts/dev-services.sh start <service>
  scripts/dev-services.sh stop <service>
  scripts/dev-services.sh status <service>
  scripts/dev-services.sh start-all
  scripts/dev-services.sh stop-all
  scripts/dev-services.sh doctor
  scripts/dev-services.sh list
USAGE
}

main() {
	load_env_file
	local command="${1:-}"
	case "$command" in
	start)
		[[ $# -eq 2 ]] || { usage; exit 1; }
		start_service "$2"
		;;
	stop)
		[[ $# -eq 2 ]] || { usage; exit 1; }
		stop_service "$2"
		;;
	status)
		[[ $# -eq 2 ]] || { usage; exit 1; }
		require_known_service "$2"
		echo "$(service_status "$2")"
		;;
	start-all)
		local service
		for service in $(services); do
			start_service "$service"
		done
		;;
	stop-all)
		local service
		for service in $(services); do
			stop_service "$service"
		done
		;;
	doctor)
		doctor
		;;
	list)
		services
		;;
	''|help|--help|-h)
		usage
		;;
	*)
		echo "unknown command: $command" >&2
		usage
		exit 1
		;;
	esac
}

main "$@"
