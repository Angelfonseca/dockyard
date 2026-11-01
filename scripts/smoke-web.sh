#!/bin/sh
# Prueba de humo del panel web contra un daemon real, dentro de Docker.
# Solo hace lecturas y peticiones que se rechazan antes de tocar nada (no crea ni borra recursos).
# La descarga usa un archivo de prueba en una carpeta temporal de respaldos (DOCKYARD_BACKUPS).
# Uso: sh scripts/smoke-web.sh /ruta/a/dockyard
set -eu

BIN="${1:-/target/debug/dockyard}"
ADDR=127.0.0.1:7681
LOG="$(mktemp)"
BK="$(mktemp -d)"
FAIL=0
PID=""

check() { # $1 descripción, $2 obtenido, $3 esperado
    if [ "$2" = "$3" ]; then
        echo "ok    $1"
    else
        echo "FALLA $1: obtenido «$2», esperado «$3»"
        FAIL=1
    fi
}

stop() {
    [ -n "$PID" ] && kill "$PID" 2>/dev/null || true
    [ -n "$PID" ] && wait "$PID" 2>/dev/null || true
    PID=""
}
trap 'stop; rm -rf "$BK"' EXIT

# Arranca el panel (con los argumentos extra dados) y espera a que responda. Deja el token en TOKEN.
start() {
    DOCKYARD_BACKUPS="$BK" "$BIN" web -l "$ADDR" "$@" >"$LOG" 2>&1 &
    PID=$!
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        curl -s -o /dev/null "http://$ADDR/" && break
        sleep 1
    done
    TOKEN="$(sed -n 's|^Panel web: .*#t=||p' "$LOG" | head -1)"
    [ -n "$TOKEN" ] || { echo "no salió el token; log:"; cat "$LOG"; exit 1; }
}

code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

echo "payload-de-prueba" >"$BK/prueba.tar.gz"
start

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

# descarga por ticket: el archivo llega completo y el ticket solo sirve una vez
TICKET="$(curl -s -X POST -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/ticket?file=prueba.tar.gz" | sed -n 's/.*"ticket":"\([^"]*\)".*/\1/p')"
check "descarga con ticket entrega el archivo" "$(curl -s "http://$ADDR/api/download?ticket=$TICKET")" "payload-de-prueba"
check "el ticket no se puede reusar" "$(code "http://$ADDR/api/download?ticket=$TICKET")" "400"
check "archivo inexistente no se ofrece" "$(code -H "X-Dockyard-Token: $TOKEN" -X POST "http://$ADDR/api/ticket?file=nada.tar.gz")" "400"

# enlace temporal: vale, y un token inventado no
LINK="$("$BIN" web-link --hours 1 -l "$ADDR" | sed -n 's|.*#t=||p')"
check "enlace temporal → 200" "$(code -H "X-Dockyard-Token: $LINK" "http://$ADDR/api/state")" "200"
check "token inventado → 401" "$(code -H "X-Dockyard-Token: inventado-0000000000000000000000" "http://$ADDR/api/state")" "401"

# bloqueo por IP: tras 4 fallos más, 429 aun con el token bueno
for _ in 1 2 3 4 5; do code -H 'X-Dockyard-Token: mal' "http://$ADDR/api/state" >/dev/null; done
check "bloqueo tras fallos repetidos → 429" "$(code -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/state")" "429"

# rotación: reiniciar con --rotate-token invalida el token anterior
OLD="$TOKEN"
stop
start --rotate-token
check "token anterior tras rotar → 401" "$(code -H "X-Dockyard-Token: $OLD" "http://$ADDR/api/state")" "401"
check "token nuevo tras rotar → 200" "$(code -H "X-Dockyard-Token: $TOKEN" "http://$ADDR/api/state")" "200"

stop
if [ "$FAIL" -eq 0 ]; then echo "todas las comprobaciones pasaron"; else echo "hubo fallos; log del servidor:"; cat "$LOG"; fi
exit "$FAIL"
