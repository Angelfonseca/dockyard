#!/bin/sh
# Clippy y pruebas unitarias dentro de un contenedor Rust: nada se compila en el equipo.
# El repo entra en solo lectura; target/ y la caché de cargo viven en volúmenes de Docker.
# Uso: scripts/test-docker.sh [imagen]     (por defecto rust:latest)
set -eu

IMG="${1:-rust:latest}"
SRC="$(cd "$(dirname "$0")/.." && pwd)"

docker run --rm \
  -v "$SRC":/src:ro \
  -v dockyard-target:/target \
  -v dockyard-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -w /src "$IMG" sh -c '
    set -e
    rustup component add clippy >/dev/null 2>&1 || true
    cargo clippy --all-targets -- -D warnings
    cargo test --bins
  '
