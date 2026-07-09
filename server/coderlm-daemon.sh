#!/usr/bin/env bash
#
# coderlm-daemon.sh — Start/stop/restart the coderlm-server as a background daemon.
#
# Usage:
#   ./server/coderlm-daemon.sh start [OPTIONS]
#   ./server/coderlm-daemon.sh stop
#   ./server/coderlm-daemon.sh restart [OPTIONS]
#   ./server/coderlm-daemon.sh status
#   ./server/coderlm-daemon.sh logs [-f]
#
# Options (passed through to coderlm-server):
#   --port PORT           Port to listen on (default: 3000)
#   --bind ADDR           Bind address (default: 127.0.0.1)
#   --max-projects N      Max concurrent indexed projects (default: 5)
#   --disable-watcher     Index projects once and refresh manually
#   --project PATH        Project directory to pre-index
#
# Environment:
#   CODERLM_DIR           Override server directory (default: auto-detect from script location)
#   CODERLM_LOG_DIR       Override log directory (default: ~/.local/state/coderlm)
#   CODERLM_PID_DIR       Override PID directory (default: ~/.local/state/coderlm)
#   CODERLM_DISABLE_WATCHER=1  Disable filesystem watchers

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CODERLM_DIR="${CODERLM_DIR:-$(dirname "$SCRIPT_DIR")/server}"
LOG_DIR="${CODERLM_LOG_DIR:-$HOME/.local/state/coderlm}"
PID_DIR="${CODERLM_PID_DIR:-$HOME/.local/state/coderlm}"
PID_FILE="$PID_DIR/coderlm-server.pid"
LOG_FILE="$LOG_DIR/coderlm-server.log"
BINARY="$CODERLM_DIR/target/release/coderlm-server"

ensure_dirs() {
    mkdir -p "$LOG_DIR" "$PID_DIR"
}

check_binary() {
    if [[ ! -x "$BINARY" ]]; then
        echo "Binary not found at $BINARY"
        echo "Build it first: cd $CODERLM_DIR && cargo build --release"
        exit 1
    fi
}

get_pid() {
    if [[ -f "$PID_FILE" ]]; then
        cat "$PID_FILE"
    fi
}

is_running() {
    local pid
    pid="$(get_pid)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        return 0
    fi
    return 1
}

cmd_start() {
    ensure_dirs
    check_binary

    if is_running; then
        echo "coderlm-server is already running (PID $(get_pid))"
        exit 0
    fi

    # Parse our options, pass the rest through to the server
    local port="3000"
    local bind="127.0.0.1"
    local max_projects="5"
    local disable_watcher=""
    local project=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --port)       port="$2"; shift 2 ;;
            --bind)       bind="$2"; shift 2 ;;
            --max-projects) max_projects="$2"; shift 2 ;;
            --disable-watcher) disable_watcher="1"; shift ;;
            --project)    project="$2"; shift 2 ;;
            *)            echo "Unknown option: $1"; exit 1 ;;
        esac
    done

    local args=(serve --port "$port" --bind "$bind" --max-projects "$max_projects")
    if [[ -n "$disable_watcher" ]]; then
        args+=(--disable-watcher)
    fi
    if [[ -n "$project" ]]; then
        args+=("$project")
    fi

    echo "Starting coderlm-server on $bind:$port ..."
    nohup "$BINARY" "${args[@]}" >> "$LOG_FILE" 2>&1 &
    local pid=$!
    echo "$pid" > "$PID_FILE"

    # Wait briefly and check it actually started
    sleep 0.5
    if kill -0 "$pid" 2>/dev/null; then
        echo "coderlm-server started (PID $pid)"
        echo "Logs: $LOG_FILE"
    else
        echo "coderlm-server failed to start. Check logs:"
        echo "  tail -20 $LOG_FILE"
        rm -f "$PID_FILE"
        exit 1
    fi
}

cmd_stop() {
    if ! is_running; then
        echo "coderlm-server is not running"
        rm -f "$PID_FILE"
        exit 0
    fi

    local pid
    pid="$(get_pid)"
    echo "Stopping coderlm-server (PID $pid) ..."
    kill "$pid"

    # Wait up to 5 seconds for graceful shutdown
    local i=0
    while kill -0 "$pid" 2>/dev/null && [[ $i -lt 10 ]]; do
        sleep 0.5
        i=$((i + 1))
    done

    if kill -0 "$pid" 2>/dev/null; then
        echo "Forcing kill ..."
        kill -9 "$pid" 2>/dev/null || true
    fi

    rm -f "$PID_FILE"
    echo "coderlm-server stopped"
}

cmd_restart() {
    cmd_stop
    cmd_start "$@"
}

cmd_status() {
    if is_running; then
        local pid
        pid="$(get_pid)"
        echo "coderlm-server is running (PID $pid)"

        # Try to hit the health endpoint
        if command -v curl &>/dev/null; then
            local health
            health="$(curl -s --max-time 2 http://127.0.0.1:3000/api/v1/health 2>/dev/null)" || true
            if [[ -n "$health" ]]; then
                echo "Health: $health"
            fi
        fi
    else
        echo "coderlm-server is not running"
        rm -f "$PID_FILE"
        exit 1
    fi
}

cmd_logs() {
    if [[ ! -f "$LOG_FILE" ]]; then
        echo "No log file at $LOG_FILE"
        exit 1
    fi

    if [[ "${1:-}" == "-f" ]]; then
        tail -f "$LOG_FILE"
    else
        tail -50 "$LOG_FILE"
    fi
}

case "${1:-help}" in
    start)   shift; cmd_start "$@" ;;
    stop)    cmd_stop ;;
    restart) shift; cmd_restart "$@" ;;
    status)  cmd_status ;;
    logs)    shift; cmd_logs "${1:-}" ;;
    help|*)
        echo "Usage: $0 {start|stop|restart|status|logs} [OPTIONS]"
        echo ""
        echo "Commands:"
        echo "  start   [--port N] [--bind ADDR] [--max-projects N] [--disable-watcher] [--project PATH]"
        echo "  stop    Stop the daemon"
        echo "  restart Stop then start with new options"
        echo "  status  Check if running + health"
        echo "  logs    Show recent logs (add -f to follow)"
        ;;
esac
