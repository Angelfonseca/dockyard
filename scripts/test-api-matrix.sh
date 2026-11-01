#!/bin/sh
# Pruebas de integración contra daemons Docker de otras versiones: la API cambió en la 1.52 y el código
# tiene caminos distintos según la versión negociada. Cada daemon corre en su contenedor docker:<v>-dind,
# en una red privada, y las pruebas se conectan con DOCKER_HOST. Tu daemon no se usa ni se publica nada en tu equipo.
#
# Uso: scripts/test-api-matrix.sh [versiones...]     (por defecto: 25 27 29)
# Requiere permisos para contenedores --privileged (los daemons anidados lo necesitan).
set -u

SRC="$(cd "$(dirname "$0")/.." && pwd)"
VERSIONS="${*:-25 27 29}"
NET=dytest-matrix
FAIL=0

docker network create "$NET" >/dev/null 2>&1 || true

for v in $VERSIONS; do
    D="dytest-dind-$v"
    docker rm -f -v "$D" >/dev/null 2>&1 || true
    if ! docker run -d --privileged --name "$D" --network "$NET" -e DOCKER_TLS_CERTDIR= "docker:$v-dind" >/dev/null; then
        echo "FALLA no se pudo arrancar docker:$v-dind"
        FAIL=1
        continue
    fi

    echo "== Docker $v"
    if docker run --rm --network "$NET" \
        -v "$SRC":/src:ro -v dy-target:/target -v dy-cargo:/usr/local/cargo/registry \
        -e CARGO_TARGET_DIR=/target -e DOCKYARD_IT=1 -e DOCKYARD_IT_AISLADO=1 -e HOME=/tmp/h \
        -e DOCKER_HOST="tcp://$D:2375" -w /src rust:latest sh -c '
            set -e
            mkdir -p /tmp/h
            apt-get update -qq >/dev/null 2>&1
            apt-get install -y -qq docker.io git >/dev/null 2>&1
            for i in $(seq 60); do docker info >/dev/null 2>&1 && break; sleep 2; done
            echo "daemon: $(docker version --format "API {{.Server.APIVersion}}, Docker {{.Server.Version}}")"
            rc=0; cargo test --test integracion -- --test-threads=1 --nocapture >/tmp/it.log 2>&1 || rc=$?
            grep -E "^test |omitida|test result|panicked|falló" /tmp/it.log
            [ $rc -eq 0 ] || tail -80 /tmp/it.log
            exit $rc
        '
    then
        echo "ok Docker $v"
    else
        echo "FALLA Docker $v"
        FAIL=1
    fi
    docker rm -f -v "$D" >/dev/null 2>&1 || true
done

docker network rm "$NET" >/dev/null 2>&1 || true
exit "$FAIL"
