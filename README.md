# dockyard

Gestor de Docker para terminal, rápido y ligero: una sola binaria (~3 MB), sin dependencias en runtime, escrita en Rust.
Panel en vivo, catálogo de plantillas (bases de datos, apps, stacks), edición segura de variables/puertos/volúmenes, logs,
shell y comandos (del contenedor y del host), respaldos y restauración, uso de disco y limpieza, Compose y Git.

Habla con el daemon por su API, así que funciona con Docker, Docker Desktop, Docker rootless y Podman, local o remoto
(`ssh://`, `tcp://`). El CLI `docker`, `docker compose` y `git` son **opcionales**: solo los usan las funciones que los
necesitan, y `dockyard doctor` dice qué falta.

## Instalación en Arch

```bash
sudo pacman -S --needed rust base-devel docker      # rust solo para compilar
sudo pacman -S --needed docker-compose git          # opcionales: Compose y Git
git clone https://github.com/TU_USUARIO/dockyard.git && cd dockyard
cargo build --release
sudo install -m755 target/release/dockyard /usr/local/bin/
sudo usermod -aG docker $USER                       # y volver a iniciar sesión
```

En macOS: `brew install rust` y el mismo `cargo build --release`. Sin `sudo install`: copia `target/release/dockyard` a donde quieras.
Actualizar: `git pull && cargo build --release`.

Si prefieres no compilar en el servidor, compila en tu otro equipo y copia solo `target/release/dockyard` (binario x86_64 glibc).

Comprobar el entorno: `dockyard doctor`.

## Uso

`dockyard` sin argumentos abre la interfaz. Pulsa `?` dentro para ver todas las teclas.

| Pestaña | Para qué |
|---|---|
| 1 Panel | CPU/RAM/disco/red/temperatura del host y de cada contenedor, con histórico |
| 2 Contenedores | logs, iniciar/detener, editar, shell, comandos, archivos, respaldar/restaurar, actualizar, borrar |
| 3 Imágenes | descargar, crear contenedor desde una imagen, borrar, limpiar sin uso |
| 4 Volúmenes | tamaño, explorar, editar archivos, respaldar/restaurar, clonar, borrar, limpiar huérfanos |
| 5 Catálogo | instalar plantillas (42 incluidas), traer desde Git, levantar proyectos Compose |
| 6 Espacio | uso de disco de Docker y limpieza granular con vista previa |

Teclas globales: `X` shell de la máquina · `!` comando en la máquina con salida en vivo · `F5` actualizar · `q` salir.

Contenedores: `Enter` logs · `s` iniciar/detener · `r` reiniciar · `e` editar · `x` shell · `c` comando · `F` editar archivo ·
`u` actualizar · `b` respaldo · `R` restaurar · `C` contraseñas · `i` inspeccionar · `d` borrar · `D` borrar stack · `/` filtrar.

### Terminal

- **`x` shell del contenedor**: bash si existe, si no sh. Va por la API con TTY real (redimensiona, Ctrl+C llega al
  contenedor), así que no necesita el CLI y funciona con daemons remotos. `dockyard shell NOMBRE [-- programa]` desde la CLI.
- **`c` comando**: salida en vivo; `Esc` lo detiene. `↑`/`↓` recorren el historial de ese contenedor.
- **`X` / `!`**: shell o comando en la máquina. Si `DOCKER_HOST=ssh://usuario@servidor`, abre la shell del servidor por SSH.
- Si la imagen no trae shell (distroless/scratch) se avisa claramente; `F` permite editar archivos igual.

### Editar un contenedor (`e`)

Cambias variables de entorno, puertos, volúmenes, imagen, reinicio, límite de memoria, **límite de CPU** o comando.
Al aplicar, dockyard **recrea** el contenedor (Docker no permite cambiar esto en caliente): detiene y renombra el viejo,
crea el nuevo con la misma configuración, lo inicia y, si algo falla, restaura el viejo. Los volúmenes (incluidos los
anónimos) se conservan. Avisa si un puerto ya está ocupado.

### Editar archivos (`F`, `f`)

Baja el archivo por la API, lo abre en tu editor (`$VISUAL`, `$EDITOR`, `nano` o `vi`) y lo sube conservando permisos y
dueño. Funciona con contenedores detenidos y dentro de volúmenes sueltos. Si el archivo no existe se crea (la carpeta sí
debe existir). CLI: `dockyard edit NOMBRE /ruta` y `dockyard vol edit VOLUMEN ruta`.

### Plantillas

Bases de datos: PostgreSQL, PostgreSQL + pgvector, MariaDB, MySQL, MongoDB, Redis, Valkey, Memcached, InfluxDB, ClickHouse,
Meilisearch, Qdrant, NATS. Administradores: Adminer, phpMyAdmin, pgAdmin, RedisInsight. Apps: Nextcloud (+MariaDB+Redis+cron),
WordPress, Gitea, Keycloak, n8n, Vaultwarden, Uptime Kuma, File Browser, Syncthing, Jellyfin, MinIO, RabbitMQ, Ollama,
Open WebUI. Red: Caddy, Traefik, Nginx, Cloudflared, Dozzle, Grafana, Prometheus, whoami. Juegos: Minecraft. Y un contenedor personalizado.

Las contraseñas se generan al azar y se muestran al terminar el despliegue; después, con `C` o `dockyard creds <stack>`.
Los stacks con varios servicios comparten una red `<nombre>-net` y se nombran `nombre`, `nombre-db`, etc.; cada dependencia
con healthcheck se espera hasta que esté lista antes de arrancar el servicio principal. `D` o `dockyard down <nombre>` los
quita juntos.

Plantillas propias: `~/.config/dockyard/templates.toml` (misma estructura que `templates.toml` del repo; si el `id`
coincide reemplaza a la integrada). Campos útiles: `healthcheck`, `dump`/`dump_ext` (respaldo), `restore`, `cpus`, `mem`.

### Compose y Git

- **Compose** (`o` en Catálogo, `dockyard compose up [carpeta|archivo] [-n proyecto]`): delega en `docker compose`.
  El proyecto aparece como stack; `D`/`dockyard down` lo quitan por API (contenedores y redes) aunque no haya CLI.
  `u` sobre uno de sus servicios hace `compose pull` + `up -d`.
- **Git** (`r` en Catálogo con `URL[#rama]`, `dockyard git URL [-r rama] [-n nombre] [--subdir carpeta]`): clona en
  `~/.local/share/dockyard/src/<nombre>`. Si trae `compose.yaml` lo levanta; si trae `Dockerfile` construye la imagen
  (`docker build` si existe, si no por la API) y abre el formulario con los puertos de su `EXPOSE`.
  Luego `u` hace `git pull` + build y recrea **solo si la imagen cambió**.
  **Repos privados (https)**: `dockyard git https://USUARIO:TOKEN@github.com/org/repo` (también pegándolo en `r`), o
  `--user`/`--token`; mejor con `DOCKYARD_GIT_USER`/`DOCKYARD_GIT_TOKEN` para que no quede en `ps` ni en el historial.
  Con solo `TOKEN@` el usuario es `git` (vale en GitHub/GitLab; Bitbucket pide usuario real). El token nunca va en la URL
  guardada, en etiquetas ni en `.git/config`: tras el primer éxito queda en `~/.local/share/dockyard/git-credentials`
  (modo 0600, texto plano, un token por host) para que `u` funcione. Bórralo para olvidarlo. Por SSH se usan tus llaves.

### Variables de entorno

Pestaña **Entorno** del detalle de un contenedor (web): tabla con buscar, secretos ocultos, agregar, quitar (con deshacer),
**pegar un `.env`** (acepta `export`, comillas y comentarios), copiar como `.env`, y marcas de nueva/modificada/se quita.
Al aplicar se recrea el contenedor y se conservan sus volúmenes. En la terminal: `e` en la TUI o
`dockyard env NOMBRE --set CLAVE=valor --unset OTRA`.

### Túneles públicos (sandbox)

Para probar un contenedor desde internet (webhooks, una demo, otro dispositivo) sin abrir puertos en el router:

```bash
dockyard tunnel up miapp                       # Cloudflare Quick Tunnel: sin cuenta, URL *.trycloudflare.com
dockyard tunnel up miapp -p 8080 --hours 4     # puerto y duración (por defecto 2 h; 0 = sin límite)
DOCKYARD_NGROK_AUTHTOKEN=... dockyard tunnel up miapp --provider ngrok   # con usuario y contraseña generados
dockyard tunnel ls                             dockyard tunnel down miapp
```

En la web: pestaña **Túnel** del contenedor. Solo HTTP(S). El túnel es otro contenedor (`dockyard-tunnel-<nombre>-<puerto>`)
sin privilegios (`cap_drop ALL`, `no-new-privileges`, 128 MB) y sin reinicio automático. Es **exposición pública**:
pide confirmación, solo abre puertos que el contenedor declara, **caduca solo** y se cierra si el contenedor destino
se recrea (su dirección podría ser ya de otro). Cloudflare no pide contraseña; ngrok sí (se genera). Usa datos de prueba.

### Respaldos y restauración

- Base de datos: `b` (usa `pg_dump`, `mariadb-dump`, `mongodump`… según la plantilla) → `~/dockyard-backups/<nombre>-<fecha>.sql`.
  Restaurar: `R` o `dockyard restore <contenedor> [archivo]` (por defecto, el más reciente).
- Volumen: `b` en Volúmenes → `.tar.gz`. Restaurar: `R` (`s` sobrescribe; `l` vacía el volumen antes) o
  `dockyard restore <volumen> [archivo] [--clean]`. Se conservan dueños y permisos.
- Los archivos viajan por la API: con un daemon remoto el respaldo queda **en tu equipo**. Usa un contenedor auxiliar
  `alpine:3` (se descarga la primera vez). Detén el contenedor que use el volumen antes de restaurarlo.

### Espacio

La pestaña 6 y `dockyard df` muestran imágenes, contenedores, volúmenes (tamaño de cada uno) y caché de construcción, con
lo recuperable. Limpieza: `c` contenedores detenidos, `i` imágenes sin uso, `b` caché de build, `n` redes, `v` volúmenes
huérfanos (borra datos), siempre con confirmación. CLI: `dockyard prune --containers --images --build --networks --volumes`
(`--all` = todo menos volúmenes; `--label` filtra).

Los contenedores nuevos o recreados rotan sus logs (`10m × 3` por defecto) cuando el driver es `json-file` o `local`.
Nota: un volumen del driver `local` no tiene cuota ni se puede «redimensionar»; lo que se controla es RAM, CPU, logs y limpieza.

## CLI para scripts

```bash
dockyard ps [-a]                          dockyard stats
dockyard logs NOMBRE -n 100 [-f]          dockyard exec NOMBRE -- ls /
dockyard shell NOMBRE [-- programa]       dockyard creds STACK
dockyard catalog [ID]                     dockyard up postgres --name pg -e POSTGRES_DB=mi -p 5433:5432 -y
dockyard up pg --tag 16-alpine --dry-run  dockyard env pg --set CLAVE=valor --unset OTRA -y
dockyard update pg                        dockyard down nc [--volumes] -y
dockyard backup pg | VOLUMEN              dockyard restore pg|VOLUMEN [archivo] [--clean] -y
dockyard edit NOMBRE /ruta                dockyard df
dockyard prune --all [--volumes] [-y]     dockyard vol ls|du|clone|edit|rm …
dockyard compose up|pull [ruta] [-n P]    dockyard compose down|ps PROYECTO
dockyard git URL [-r rama] [-n nombre] [-p 8080:80] [-e K=V] [-y]
dockyard start|stop|restart|rm NOMBRE...  dockyard doctor
```

## Configuración (opcional)

`~/.config/dockyard/config.toml`

```toml
tz = "America/Mexico_City"     # TZ para contenedores nuevos
backups_dir = "/srv/backups"
refresh_secs = 2
default_restart = "unless-stopped"
log_max_size = "10m"           # "" desactiva la rotación de logs
log_max_file = 3
```

Variables: `DOCKER_HOST`, `DOCKER_CONTEXT`, `DOCKYARD_BACKUPS`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `EDITOR`.

## Conexión

Orden de búsqueda del daemon: `DOCKER_HOST` → contexto activo de `docker context` → `/var/run/docker.sock` →
Docker Desktop → rootless (`$XDG_RUNTIME_DIR/docker.sock`) → Colima/OrbStack/Rancher → Podman. La versión de la API se
negocia, así que funciona también con daemons viejos. `doctor` muestra cuál se usó.

## Panel web

`dockyard web -l IP:7681` sirve un panel en el navegador (una sola página, sin dependencias en el servidor):

| Vista | Para qué |
|---|---|
| Inicio | CPU/RAM/disco/temperatura/red del servidor, contenedores con problemas y los más activos |
| Contenedores | iniciar/detener/reiniciar; detalle con resumen y contraseñas, logs en vivo (filtrar, descargar), terminal de comandos, editar (variables, puertos, volúmenes, límites), actualizar, respaldar, eliminar; quitar stacks |
| Tienda | el catálogo (42 plantillas) con búsqueda y categorías; formulario de instalación con contraseñas generadas, puertos libres elegidos solos y a quién exponerlos (solo VPN, solo el equipo para Caddy, o toda la red) |
| Imágenes | descargar, actualizar, borrar las que no se usan |
| Volúmenes | tamaño, quién los usa, respaldar, restaurar, borrar |
| Respaldos | crear (bases de datos y volúmenes), descargar al navegador, restaurar, borrar |
| Espacio | lo que ocupa Docker y botones para liberar cada parte |

Las tareas largas (instalar, actualizar, respaldar) muestran su progreso en vivo. Git (público o privado con usuario y token)
se trae desde **Desde Git**; la shell interactiva y Compose siguen en la terminal.

```bash
dockyard web -l 100.x.y.z:7681      # IP de Tailscale: solo accesible por la VPN (por defecto 127.0.0.1:7681)
```

Muestra una URL `http://IP:7681/#t=TOKEN`; ábrela una vez y el navegador guarda el acceso. El token vive en
`~/.config/dockyard/web-token` (0600); bórralo y reinicia para invalidar los accesos. No uses `0.0.0.0`: quien entre
controla tu Docker. Los estilos van incluidos en la binaria; el navegador solo usa internet para la tipografía y los
logos de la Tienda (si faltan, hay letras y la fuente del sistema).

### Seguridad

- **Acceso**: token de 32 caracteres aleatorios que el navegador manda en la cabecera `X-Dockyard-Token`. No se usa cookie
  porque las cookies no distinguen puertos: cualquier contenedor publicado en el mismo servidor la habría recibido. El token
  entra por el fragmento `#t=` (nunca llega al servidor, a proxies ni a `Referer`) y se borra de la barra de direcciones.
- **Navegador**: `Content-Security-Policy` con nonce (solo corre el script propio, sin scripts de terceros), `frame-ancestors 'none'`,
  `nosniff`, `no-referrer`, `Cross-Origin-*`. Descargas con un ticket de un solo uso (60 s) que no lleva el token.
- **Servidor**: máximo 128 conexiones, tiempos límite de lectura, cuerpo ≤ 2 MiB, sin `Transfer-Encoding`.
- **Git**: solo `https`/`ssh`/`git` desde el panel (sin rutas locales, `file://` ni `ext::`), sin redirecciones, URL que no puede
  empezar por `-`, subcarpeta confinada al repositorio, y el token solo se entrega al host de la URL y nunca por `http`
  (salvo hacia esta misma máquina). `u` solo actualiza carpetas de `~/.local/share/dockyard/src`.
- **Repositorios**: su `Dockerfile` o `compose.yaml` corre con acceso a Docker; despliega solo repos en los que confíes.

Si cambias clases de Tailwind en `src/web.html`, regenera `src/web.css`:
`npx tailwindcss@3.4.17 -i in.css -o src/web.css --minify` con `@tailwind base; @tailwind components; @tailwind utilities;` en `in.css` y `content: ["src/web.html"]`.

Como servicio (`/etc/systemd/system/dockyard-web.service`, cambia usuario e IP):

```ini
[Unit]
Description=dockyard web
After=docker.service tailscaled.service
Wants=tailscaled.service

[Service]
User=TU_USUARIO
ExecStart=/usr/local/bin/dockyard web -l 100.x.y.z:7681
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

`sudo systemctl enable --now dockyard-web` y `journalctl -u dockyard-web` muestra la URL con el token.

## Pruebas

```bash
cargo test                                                     # unitarias, sin Docker
DOCKYARD_IT=1 cargo test --test integracion -- --test-threads=1  # contra Docker real (solo toca «dytest-it-*»)
```

La detección de puertos ocupados del formulario solo comprueba el equipo local (no aplica con daemon remoto).
