#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# Bootstrap Ollama + Qdrant via Docker Compose and pull models from .env
# Place this script in the project root next to docker-compose.yml and .env
# Works on macOS/Linux and Windows (Git Bash/WSL)
# -----------------------------------------------------------------------------

set -Eeuo pipefail

# --- Config defaults (can be overridden via flags) ---------------------------
CONTAINER_NAME="ollama"       # matches container_name in docker-compose.yml
COMPOSE_FILE_NAME="docker-compose.yml"

# --- Utility functions -------------------------------------------------------
log()   { printf "▶ %s\n" "$*"; }
ok()    { printf "✅ %s\n" "$*"; }
warn()  { printf "⚠️  %s\n" "$*\n"; }
fail()  { printf "❌ %s\n" "$*\n" >&2; exit 1; }

require_cmd() { command -v "$1" >/dev/null 2>&1 || fail "Command '$1' not found in PATH"; }

find_compose_cmd() {
  if docker compose version >/dev/null 2>&1; then
    echo "docker compose"
  elif command -v docker-compose >/dev/null 2>&1; then
    echo "docker-compose"
  else
    fail "Neither 'docker compose' nor 'docker-compose' is available. Install Docker."
  fi
}

usage() {
  cat <<EOF
Usage: $(basename "$0") [options]

Options:
  -n <name>   Ollama container_name (default: ${CONTAINER_NAME})
  -f <file>   Compose file name in script directory (default: ${COMPOSE_FILE_NAME})
  -h          Show help

Notes:
- The script runs from its own directory so docker-compose resolves the local .env.
- Models to pull are read from .env: OLLAMA_MODEL, EMBEDDING_MODEL.
EOF
}

# --- Parse flags -------------------------------------------------------------
while getopts ":n:f:h" opt; do
  case "${opt}" in
    n) CONTAINER_NAME="${OPTARG}" ;;
    f) COMPOSE_FILE_NAME="${OPTARG}" ;;
    h) usage; exit 0 ;;
    \?) fail "Unknown option: -$OPTARG" ;;
    :)  fail "Option -$OPTARG requires an argument" ;;
  esac
done

# --- Resolve script directory and move there ---------------------------------
# This guarantees docker compose picks up the local docker-compose.yml and .env
SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &>/dev/null && pwd )"
cd "$SCRIPT_DIR"

# --- Pre-flight checks --------------------------------------------------------
require_cmd docker
COMPOSE_CMD="$(find_compose_cmd)"

[[ -f "$COMPOSE_FILE_NAME" ]] || fail "Compose file not found: $COMPOSE_FILE_NAME"
[[ -f ".env" ]] || fail ".env not found next to the script"

# --- Load .env (only the variables we care about) ----------------------------
# We avoid exporting everything to reduce side effects.
# Safe to source: no command substitution, only KEY=VALUE lines.
# shellcheck disable=SC1091
set -a
source ".env"
set +a

# Gather models from .env (skip empty / unset)
MODELS=()
[[ -n "${EMBEDDING_MODEL:-}" ]] && MODELS+=("$EMBEDDING_MODEL")
[[ -n "${OLLAMA_MODEL:-}"    ]] && MODELS+=("$OLLAMA_MODEL")

if [[ ${#MODELS[@]} -eq 0 ]]; then
  warn "No models defined in .env (EMBEDDING_MODEL / OLLAMA_MODEL). Nothing to pull."
fi

# --- Bring up Compose stack ---------------------------------------------------
log "Starting services via Compose..."
if [[ "$COMPOSE_CMD" == "docker compose" ]]; then
  docker compose -f "$COMPOSE_FILE_NAME" up -d
else
  docker-compose -f "$COMPOSE_FILE_NAME" up -d
fi
ok "Compose stack is up (detached)."

# --- Wait for container to be Running ----------------------------------------
log "Waiting for container '${CONTAINER_NAME}' to be Running..."
for i in {1..180}; do
  state="$(docker inspect -f '{{.State.Running}}' "$CONTAINER_NAME" 2>/dev/null || true)"
  [[ "$state" == "true" ]] && { ok "Container is Running."; break; }
  sleep 1
  [[ $i -eq 180 ]] && fail "Container '${CONTAINER_NAME}' did not reach Running state in time."
done

# --- Wait for Ollama API readiness inside the container ----------------------
# We override OLLAMA_HOST for docker exec only. Inside the container, the
# server listens on 0.0.0.0:11434, but the client must call via 127.0.0.1.
log "Waiting for Ollama API to become ready..."
for i in {1..180}; do
  if docker exec -e OLLAMA_HOST=http://127.0.0.1:11434 "$CONTAINER_NAME" ollama list >/dev/null 2>&1; then
    ok "Ollama is ready."
    break
  fi
  sleep 1
  [[ $i -eq 180 ]] && fail "Ollama did not respond to 'ollama list' in time."
done

# --- Helper: check if a model is already installed ---------------------------
is_model_installed() {
  local model="$1"
  # Use 'ollama list' and compare against the first column (NAME).
  docker exec -e OLLAMA_HOST=http://127.0.0.1:11434 "$CONTAINER_NAME" \
    sh -c "ollama list | awk 'NR>1 {print \$1}' | grep -Fxq \"$model\""
}

# --- Pull models if missing ---------------------------------------------------
if [[ ${#MODELS[@]} -gt 0 ]]; then
  log "Ensuring models are present: ${MODELS[*]}"
fi

for model in "${MODELS[@]:-}"; do
  echo
  log "Checking model: $model"
  if is_model_installed "$model"; then
    ok "Already installed: $model"
  else
    log "Pulling: $model"
    # Stream pull output to the console; do not use -t to keep plain output.
    if docker exec -e OLLAMA_HOST=http://127.0.0.1:11434 "$CONTAINER_NAME" ollama pull "$model"; then
      ok "Model ready: $model"
    else
      warn "Failed to pull: $model. Verify the model name/tag (case-sensitive in some registries)."
    fi
  fi
done

echo
ok "All done."
