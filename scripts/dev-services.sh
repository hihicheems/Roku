#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR="${ROKU_RUN_DIR:-$ROOT_DIR/run/dev-services}"
LOG_DIR="${ROKU_DEV_SERVICE_LOG_DIR:-$ROOT_DIR/logs/dev-services}"

readonly ROOT_DIR RUN_DIR LOG_DIR

services() {
	printf '%s\n' "telegram-bot"
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
	telegram-bot)
		printf '%s\n' "cargo run -p roku-cmd -- telegram-bot"
		;;
	*)
		echo "unknown service: ${1:-}" >&2
		return 1
		;;
	esac
}

service_pid_file() {
	printf '%s/%s.pid\n' "$RUN_DIR" "$1"
}

service_started_at_file() {
	printf '%s/%s.started_at\n' "$RUN_DIR" "$1"
}

service_stdout_log() {
	printf '%s/%s.stdout.log\n' "$LOG_DIR" "$1"
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

	local command stdout_log pid_file started_at_file
	command="$(service_command "$service")"
	stdout_log="$(service_stdout_log "$service")"
	pid_file="$(service_pid_file "$service")"
	started_at_file="$(service_started_at_file "$service")"

	(
		cd "$ROOT_DIR"
		nohup /bin/bash -lc "$(load_env_prefix) cd '$ROOT_DIR'; exec ${command}" \
			>>"$stdout_log" 2>&1 &
		local pid=$!
		printf '%s\n' "$pid" > "$pid_file"
		date +%s > "$started_at_file"
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

doctor() {
	ensure_layout
	printf '%-18s %-5s %-10s %-8s %-10s %s\n' "NAME" "READY" "STATUS" "PID" "UPTIME" "LOG"
	for service in $(services); do
		local status ready pid uptime log_path
		status="$(service_status "$service")"
		log_path="$(service_stdout_log "$service")"
		if [[ "$status" == "Running" ]]; then
			ready="1/1"
			pid="$(service_pid "$service")"
			uptime="$(format_duration "$(elapsed_seconds "$service")")"
		else
			ready="0/1"
			pid="-"
			uptime="-"
		fi

		printf '%-18s %-5s %-10s %-8s %-10s %s\n' \
			"$service" \
			"$ready" \
			"$status" \
			"$pid" \
			"$uptime" \
			"$log_path"
	done
}

usage() {
	cat <<'EOF'
Usage:
  scripts/dev-services.sh start <service>
  scripts/dev-services.sh stop <service>
  scripts/dev-services.sh status <service>
  scripts/dev-services.sh start-all
  scripts/dev-services.sh stop-all
  scripts/dev-services.sh doctor
  scripts/dev-services.sh list
EOF
}

main() {
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
		for service in $(services); do
			start_service "$service"
		done
		;;
	stop-all)
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
	"" | help | --help | -h)
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
