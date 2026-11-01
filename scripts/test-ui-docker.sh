#!/bin/sh
# Prueba de humo en un navegador real (Playwright) dentro de Docker: compila el panel, lo arranca con el
# socket de Docker en solo lectura de peticiones y recorre todas sus vistas. No crea ni borra recursos.
# Uso: scripts/test-ui-docker.sh [imagen_playwright]
set -eu

SRC="$(cd "$(dirname "$0")/.." && pwd)"
PW="${1:-mcr.microsoft.com/playwright:v1.56.0-noble}"

docker run --rm -v "$SRC":/src:ro -v dy-target:/target -v dy-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/target -w /src rust:latest cargo build -q

docker run --rm -v "$SRC":/src:ro -v /var/run/docker.sock:/var/run/docker.sock -v dy-target:/target:ro \
    -e HOME=/tmp/h -w /src "$PW" sh -c '
        set -e
        mkdir -p /tmp/h
        /target/debug/dockyard web -l 127.0.0.1:7681 >/tmp/web.log 2>&1 &
        PID=$!
        for i in $(seq 30); do grep -q "Panel web" /tmp/web.log 2>/dev/null && break; sleep 1; done
        TOKEN="$(sed -n "s|^Panel web: .*#t=||p" /tmp/web.log | head -1)"
        [ -n "$TOKEN" ] || { cat /tmp/web.log; kill $PID; exit 1; }
        npm install -s --prefix /tmp/pw playwright@1.56.0 >/tmp/npm.log 2>&1 || { cat /tmp/npm.log; kill $PID; exit 1; }
        rc=0
        NODE_PATH=/tmp/pw/node_modules node /src/scripts/ui-smoke.js http://127.0.0.1:7681 "$TOKEN" || rc=$?
        kill $PID 2>/dev/null || true
        exit $rc
    '
