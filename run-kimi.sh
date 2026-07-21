#!/usr/bin/env bash
set -euo pipefail

COMPOSE_FILE=".docker/compose.yaml"
SERVICE_NAME="kimi"
PROJECT_DIR="src"

show_help() {
  cat <<EOF
Usage:
  ./run-kimi.sh [command] [args...]

Runs commands inside the Kimi Docker container.

Commands:
  help, -h, --help
      Show this help message.

  shell
      Open an interactive shell in the container.

  make [target...]
      Run make inside the Rust project directory (${PROJECT_DIR}).
      Example:
        ./run-kimi.sh make dist
        ./run-kimi.sh make build
        ./run-kimi.sh make test

  cargo [args...]
      Run cargo inside the Rust project directory (${PROJECT_DIR}).
      Example:
        ./run-kimi.sh cargo test
        ./run-kimi.sh cargo build --release

  <command> [args...]
      Run any other command from the container workspace root.
      Example:
        ./run-kimi.sh ls -la
        ./run-kimi.sh bash -lc 'cd src && cargo test'

If no command is provided, an interactive container session is started.
EOF
}

run_container() {
  docker compose --file "$COMPOSE_FILE" run --rm "$SERVICE_NAME" "$@"
}

docker compose --file "$COMPOSE_FILE" build "$SERVICE_NAME"

if [ "$#" -eq 0 ]; then
  run_container
  exit 0
fi

case "$1" in
  help|-h|--help)
    show_help
    ;;

  shell)
    run_container bash
    ;;

  make)
    shift
    run_container make -C "$PROJECT_DIR" "$@"
    ;;

  cargo)
    shift
    run_container bash -lc "cd '$PROJECT_DIR' && cargo $(printf '%q ' "$@")"
    ;;

  *)
    run_container "$@"
    ;;
esac
