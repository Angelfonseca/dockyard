#!/bin/sh
# Prueba de humo del panel web contra un daemon real, dentro de Docker.
# Solo hace lecturas y peticiones que se rechazan antes de tocar nada (no crea ni borra recursos).
# Uso: docker run ... (ver scripts/test-docker.sh) o, dentro de un contenedor con Docker y curl:
#   sh /src/scripts/smoke-web.sh /ruta/a/dockyard
set -eu

BIN="${1:-/target/debug/dockyard}"
ADDR=127.0.0.1:7681
LOG="$(mktemp)"
FAIL=0

check() { # $1 descripción, $2 valor obtenido, $3 valor esperado
    if [ "$2" = "$3" ]; then
        echo "ok    $1"
    else
        echo "FALLA $1: obtenido «$2», esperado «$3»"
        FAIL=1
    fi
}

"$BIN" web -l "$ADDR" >"$LOG" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for _ in 1 2 3 4 5 6 7 8 9 10; do
    curl -s -o /dev/null "http://$ADDR/" && break
    sleep 1
done
TOKEN="$(sed -n 's|^Panel web: .*#t=||p' "$LOG" | head -1)"
[ -n "$TOKEN" ] || { echo "no salió el token; log:"; cat "$LOG"; exit 1; }

code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

check "página pública sirve 200" "$(code "http://$ADDR/")" "200"
check "la página lleva la CSP con nonce" "$(curl -s -D - -o /dev/null "http://$ADDR/" | grep -ci '^content-security-policy: .*nonce-')" "1"
check "cabeceras de seguridad presentes" "$(curl -s -D - -o /dev/null "http://$ADDR/" | grep -ci '^x-frame-options: DENY')" "1"
check "API sin token → 401" "$(code "http://$ADDR/api/state")" "401"
check "Host de otro nombre → 403" "$(code -H 'Host: evil.example:7681' "http://$ADDR/")" "403"
check "Origin ajeno → 403" "$(code -H 'Origin: https://evil.example' -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/state")" "403"
check "con token → 200" "$(code -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/state")" "200"
check "Transfer-Encoding no admitido → 400" "$(code -H 'Transfer-Encoding: chunked' -H "X-Dockyard-Token: $TOKEN" -X POST "http://$ADDR/api/prune?what=images")" "400"
check "cuerpo > 256 KB → 413" "$(head -c 300000 /dev/zero | code -H "X-Dockyard-Token: $TOKEN" -H 'Content-Type: application/json' --data-binary @- "http://$ADDR/api/deploy")" "413"
check "acción desconocida → 400 (no toca nada)" "$(code -H "X-Dockyard-Token: $TOKEN" -X POST "http://$ADDR/api/prune?what=nada")" "400"

# keep-alive: dos peticiones por la misma conexión
REUSE="$(curl -sv -o /dev/null -o /dev/null -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/state" "http://$ADDR/api/state" 2>&1 | grep -c 'Re-using existing' || true)"
check "keep-alive reutiliza la conexión" "$REUSE" "1"

# bloqueo por IP: tras 4 fallos más, 429 aun con el token bueno
for _ in 1 2 3 4 5; do code -H 'X-Dockyard-Token: mal' "http://$ADDR/api/state" >/dev/null; done
check "bloqueo tras fallos repetidos → 429" "$(code -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/state")" "429"

if [ "$FAIL" -eq 0 ]; then echo "todas las comprobaciones pasaron"; else echo "hubo fallos; log del servidor:"; cat "$LOG"; fi
exit "$FAIL"
