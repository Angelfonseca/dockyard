# Plan de mejora de dockyard

Objetivo: que dockyard sea **a prueba de todo** y cubra el ciclo completo de un servidor casero:
traer contenedores (catálogo, imagen, Compose, Git), operarlos (terminal del contenedor y del host),
ver y administrar espacio y volúmenes, y respaldar/restaurar sin perder datos.

## Principios

1. **API primero.** Todo lo esencial habla con el daemon por su socket (unix, tcp o ssh) mediante bollard.
   Funciona con Docker, Docker Desktop, Docker rootless y Podman (API compatible), local o remoto.
2. **CLI opcional.** `docker`, `docker compose` y `git` se aprovechan si están instalados; si faltan,
   solo se desactiva la función que los necesita y se avisa qué instalar. `dockyard doctor` lo reporta.
3. **Nunca perder datos.** Toda operación destructiva pide confirmación, toda recreación es reversible,
   los volúmenes se conservan salvo que se pida lo contrario.
4. **Probado contra un daemon real.** Pruebas unitarias sin Docker + pruebas de integración contra Docker
   (`DOCKYARD_IT=1 cargo test`), que solo tocan recursos con prefijo `dytest-`.

## Diagnóstico inicial

| Hallazgo | Impacto |
|---|---|
| 2 de 19 pruebas fallan sin daemon (`ui.rs` hace `Engine::connect().expect`) | CI imposible sin Docker |
| Solo busca `/var/run/docker.sock` (o `DOCKER_HOST`) | No conecta en Docker Desktop sin enlace, rootless, Podman ni contextos `docker context` |
| `ssh://` en `DOCKER_HOST` no compila (falta la feature `ssh` de bollard) | No sirve para servidores remotos por SSH |
| Si el daemon se cuelga, el recolector espera para siempre | La TUI se congela sin avisar |
| Respaldo/restauración de volúmenes montan una carpeta local | Con daemon remoto el archivo queda en el servidor remoto |
| Logs `json-file` sin límite | Llenan el disco con el tiempo |
| Los stacks arrancan la app sin esperar a la base de datos | Primer arranque con reinicios en bucle |
| Hay respaldo SQL pero no restauración | Un respaldo que no se restaura no es respaldo |
| La restauración de volumen no borra archivos sobrantes | Restauración «sucia» |
| Las contraseñas solo se ven al desplegar | Se pierden si no se anotan |
| La shell `x` depende del binario `docker` | Falla sin CLI o con daemon remoto sin CLI |

## Fase 0 — Robustez

- [x] **0.1 Pruebas sin Docker**: el test de UI crea un cliente que no abre el socket.
- [x] **0.2 Descubrimiento del daemon**: `DOCKER_HOST` → contexto actual de `docker context`
      (`~/.docker/config.json` + `contexts/meta/*/meta.json`) → `/var/run/docker.sock` →
      `~/.docker/run/docker.sock` (Desktop) → `$XDG_RUNTIME_DIR/docker.sock` (rootless) →
      sockets de Podman. Se activa la feature `ssh` de bollard para `ssh://usuario@host`.
      El host elegido se pasa a los procesos hijos (`docker compose`, `docker build`) para que todos
      hablen con el mismo daemon.
- [x] **0.2b Versión de la API**: se negocia al conectar y tras cada reconexión (bollard usa 1.53 fijo; con
      Docker 27/28 todo fallaba con «client version too new»). `system df` cambió de formato en la API 1.52: con
      daemons viejos el uso se calcula con los listados y el tamaño de volúmenes con un auxiliar (`du`).
- [x] **0.3 Tiempos límite**: el recolector usa `timeout` en cada llamada; si el daemon no responde
      muestra «Docker no responde» y reintenta.
- [x] **0.4 Respaldos remotos**: el volumen se empaqueta dentro de un contenedor auxiliar y el `.tar.gz`
      viaja por la API (attach) al equipo local. La restauración sube el `.tar.gz` con la API de archivos
      (el daemon lo descomprime y conserva dueños y permisos). Compatible con respaldos anteriores.
- [x] **0.5 Rotación de logs**: contenedores nuevos (y los recreados sin configuración propia) usan
      `max-size=10m`, `max-file=3` cuando el driver es `json-file` o `local`. Configurable en `config.toml`.
- [x] **0.6 Healthchecks y espera de dependencias**: las plantillas declaran `healthcheck`; al desplegar
      un stack se espera a que cada dependencia esté sana antes de arrancar el servicio principal.
- [x] **0.7 Restauración SQL**: plantillas con `restore` (psql, mariadb, mysql, mongorestore);
      `dockyard restore <contenedor> <archivo>` y tecla `R` en la TUI.
- [x] **0.8 Restauración limpia**: `--clean` (CLI) / opción `l` (TUI) vacía el volumen antes de extraer.
- [x] **0.9 Credenciales**: `dockyard creds <stack|contenedor>` y tecla `C`; se leen de la configuración
      del contenedor, no se guardan aparte.

## Fase 1 — Terminal del contenedor y del host

- [x] **1.1 Shell nativa** (`x`, `dockyard shell NOMBRE`): exec con TTY por la API, modo raw, redimensionado
      con `SIGWINCH`. Se usa siempre la nativa (una sola ruta, sin la trampa de Ctrl+P de `docker exec`).
      Mensaje claro si la imagen no tiene shell (distroless).
- [x] **1.2 Shell del host** (`X`): suspende la TUI y abre `$SHELL`; con `DOCKER_HOST=ssh://` abre `ssh` al servidor.
- [x] **1.3 Comandos con salida en vivo**: `c` (contenedor) y `!` (host) muestran la salida mientras llega;
      `Esc` cancela (el comando del contenedor corre con TTY, así que se interrumpe al cerrar).
- [x] **1.4 Historial de comandos** por contenedor y para el host (`↑`/`↓` en el prompt).

## Fase 2 — Espacio y volúmenes

- [x] **2.1 Uso real del disco** (`system df`): tamaño por volumen, imágenes, contenedores y caché de build.
      Se calcula como mucho cada 30 s y solo con la pestaña visible.
- [x] **2.2 Pestaña Espacio** (6): totales, recuperable, volúmenes más grandes, y limpieza granular con vista
      previa: contenedores detenidos, imágenes sin uso, caché de build, redes sin uso, volúmenes huérfanos.
      CLI: `dockyard df` y `dockyard prune --containers --images --build --networks --volumes`.
- [x] **2.3 Explorar volumen** (`Enter` en Volúmenes, `dockyard vol du`): carpetas más pesadas y listado.
- [x] **2.4 Editar archivos** (`F` en Contenedores, `f` en Volúmenes, `dockyard edit`): baja el archivo con
      la API de archivos, abre `$EDITOR` y lo sube conservando permisos y dueño. Funciona con contenedores
      detenidos y con volúmenes sueltos.
- [x] **2.5 Clonar volumen** (`c` en Volúmenes, `dockyard vol clone`): copia exacta (`cp -a`).
- [x] **2.6 Límite de CPU**: campo «CPU máx.» en el formulario (`--cpus` de Docker).

Nota: el driver `local` no tiene cuotas; un volumen no se puede «redimensionar». Lo que sí se controla:
RAM, CPU, logs y limpieza.

## Fase 3 — Traer desde Compose y Git

- [x] **3.1 Compose** (`dockyard compose up|down|pull|ps`, tecla `o` en Catálogo): delega en
      `docker compose` (o `docker-compose`). Los proyectos aparecen como stack; `D` y `dockyard down`
      los eliminan por API (contenedores + redes) incluso sin CLI. `u` sobre un servicio de Compose
      usa `compose pull` + `up -d` cuando hay CLI.
- [x] **3.2 Git** (`dockyard git URL`, tecla `r` en Catálogo; `g` ya es «ir al inicio»): clona (`--depth 1`) en
      `~/.local/share/dockyard/src/<nombre>`. Con archivo Compose → `compose up`. Con `Dockerfile` →
      construye la imagen (`docker build` si hay CLI, si no por la API) y abre el formulario con los puertos
      del `EXPOSE`. El contenedor guarda origen y rama en etiquetas, y `u` hace `git pull` + rebuild + recreate.
- [x] **3.3 Más plantillas** (con healthcheck y respaldo/restauración donde aplica): PostgreSQL+pgvector,
      ClickHouse, Meilisearch, Qdrant, NATS, Keycloak+PostgreSQL, Traefik.

## Fase 4 — Confianza

- [x] **4.1 Pruebas de integración** (`tests/integracion.rs`): despliegue, edición, respaldo y restauración
      SQL y de volumen, edición de archivos, exec, clonado, Compose y Git contra un daemon real.
- [x] **4.2 CI** (GitHub Actions): `clippy -D warnings`, pruebas unitarias y de integración en Ubuntu.
- [x] **4.3 `doctor` completo**: daemon y cómo se encontró, versión de API, driver de logs, CLI de Docker,
      Compose, Git, editor, carpeta de respaldos con permiso de escritura.
- [x] **4.4 README** actualizado con todas las teclas y comandos.

## Resultado y verificación (2026-10-26, Docker Desktop 29.6.1 en macOS)

Todo lo anterior está implementado. Verificado:

- `cargo clippy --all-targets -D warnings` limpio; 30 pruebas unitarias sin Docker (antes 17/19 pasaban).
- `DOCKYARD_IT=1 cargo test --test integracion`: 2 pruebas de punta a punta contra Docker real (PostgreSQL:
  despliegue, rotación de logs, healthcheck, respaldo/restauración SQL y de volumen con dueños, `--clean`, clonar,
  explorar, `df`, editar archivos en contenedor y volumen, credenciales, variables, update; Git con Dockerfile:
  despliegue, «al día» vs «actualizado»; Compose: up, ps, down por API).
- A mano además: stack WordPress+MariaDB (WordPress arrancó 0.9 s después de que MariaDB pasó a healthy, 0 reinicios),
  respaldo/restauración MariaDB, construcción por la API sin CLI de docker, repositorio Git con compose, limpieza
  filtrada por etiqueta, shell nativa en una pty (código de salida, tamaño, `TERM`, aviso distroless).
- TUI real en una pty con teclas simuladas: pestaña Espacio, tamaños de volumen, comando en vivo en contenedor y host,
  shell `x`, shell `X`, editar archivo `F`, explorar/clonar/restaurar volumen, Compose (`o`) y Git (`r`) desde el Catálogo.

Hallazgos durante la implementación, ya resueltos: con el almacén containerd cada `docker build` cambia el ID de la
imagen (atestaciones con fecha) y la imagen vieja deja de poder inspeccionarse al reetiquetarla, así que `update`
compara una huella de capas + configuración tomada antes de construir; la tecla `g` del Catálogo chocaba con «ir al
inicio»; el aviso «¿puerto ocupado?» salía con cualquier error de arranque.

No verificado (no hay cómo en este equipo): daemon remoto por `ssh://`, Podman y un daemon con API < 1.52.
El código para esos casos está y la lógica de análisis tiene pruebas unitarias.

## Qué necesita cada función

| Función | Solo daemon | Necesita |
|---|---|---|
| Panel, contenedores, imágenes, volúmenes, logs, editar, actualizar | ✅ | — |
| Respaldos y restauración (SQL y volúmenes), espacio, limpieza | ✅ | — |
| Explorar, editar archivos, clonar volúmenes | ✅ | — (descarga `alpine:3` la primera vez) |
| Comandos y shell del contenedor | ✅ | — (no usa el CLI de docker) |
| Shell/comandos del host | ✅ | `ssh` si el daemon es remoto por SSH |
| Construir desde `Dockerfile` | ✅ | `docker build` se usa si existe (BuildKit) |
| Clonar desde Git | — | `git` |
| Compose | — | `docker compose` o `docker-compose` |
| Editar archivos | — | un editor (`$EDITOR`, `nano` o `vi`) |

## Fuera de alcance (a propósito)

UI web multiusuario (hay un panel personal: `dockyard web`), varios hosts a la vez, Kubernetes/Swarm, sistema de plugins, base de datos propia (las etiquetas
de Docker son el estado), parser propio de Compose, respaldos programados (usar `cron`/`systemd` con
`dockyard backup`), copiar archivos arbitrarios host↔contenedor (`docker cp`).
