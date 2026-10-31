# Plan de mejora (segunda ronda) — análisis del 2026-10-31

Base: `PLAN.md` (fases 0–4 completas). Este documento lista lo que queda después de revisar el código.

## Estado verificado (todo ejecutado en Docker, `rust:latest`, repo montado en solo lectura)

- `cargo clippy --all-targets -- -D warnings`: limpio.
- `cargo test --bins`: 42/42 pasan sin daemon.
- No verificado: pruebas de integración (`DOCKYARD_IT=1`), `ssh://`, Podman, API < 1.52 (igual que en PLAN.md).
- Tamaño: ~12.6k líneas de Rust. Archivos grandes: `app.rs` (2195), `engine.rs` (1801), `ui.rs` (1258), `cli.rs` (1144), `web.rs` (1102), `ext.rs` (880).

## Hallazgos

| # | Hallazgo | Impacto |
|---|---|---|
| H1 | El panel web usa HTTP plano con un solo token estático, sin límite de intentos ni caducidad. El token solo se rota borrando el archivo. | Con `--listen 0.0.0.0` o Tailscale el token viaja sin cifrar; un token filtrado da control total del daemon (`/api/exec`, `/api/deploy`, `/api/prune`) |
| H2 | Servidor HTTP escrito a mano (`handle`/`parse` en `web.rs`): sin keep-alive, sin límite de conexiones simultáneas, sin validar `Host`/`Origin`. Cada conexión puede reservar hasta `MAX_BODY` | Expuesto a DNS rebinding contra `127.0.0.1` y a agotamiento de memoria; cada petición paga un handshake nuevo |
| H3 | `git-credentials` en texto plano (0600) y el token de ngrok viaja por variable de entorno al contenedor | Aceptable para uso casero, pero conviene avisarlo en `doctor` y ofrecer alternativa |
| H4 | `Mutex::lock().unwrap()` en `web.rs` (~12 sitios) | Un pánico en un hilo envenena el mutex y tumba todo el panel; el perfil release usa `panic = "abort"` |
| H5 | `app.rs`, `engine.rs`, `cli.rs` y `web.rs` mezclan varias responsabilidades en archivos de 1–2k líneas | Cuesta añadir funciones y revisar cambios |
| H6 | Lógica duplicada entre `cli.rs`, `app.rs` y `web.rs` (despliegue, respaldo, git, prune) | Un arreglo hay que repetirlo en tres sitios |
| H7 | Las pruebas de integración solo cubren 2 escenarios grandes y corren en serie | Un fallo no dice qué función se rompió; no se ejecutan en CI local sin Docker |
| H8 | CI usa `stable` sin fijar versión y sin `cargo audit`/`cargo deny`; `Cargo.toml` sin `rust-version` | Builds no reproducibles; dependencias vulnerables sin aviso |
| H9 | Sin releases: la instalación es compilar desde fuente (README apunta a `TU_USUARIO`) | Fricción para instalar; placeholder sin reemplazar |
| H10 | Respaldos solo manuales (`cron` sugerido), sin retención ni verificación | Un respaldo corrupto se descubre al restaurar |
| H11 | Idioma único (español) en TUI, CLI y web, con cadenas incrustadas | Limita la comunidad; difícil de mantener |
| H12 | El panel web no tiene pruebas automáticas de la UI (`web.html` 1366 líneas) | Regresiones visuales y de JS pasan inadvertidas |

## Fase 5 — Seguridad del panel web (prioridad alta)

- [ ] **5.1 Validar `Host` y `Origin`** en cada petición; rechazar si no coincide con la dirección de escucha (cierra DNS rebinding y CSRF desde otra página).
- [ ] **5.2 Límite de intentos**: tras N tokens inválidos desde una IP, esperar de forma creciente (429). Registrar el intento.
- [ ] **5.3 Tokens con caducidad y rotación**: `dockyard web --rotate-token`, y opción de token de sesión que caduca (p. ej. 12 h) sin invalidar el permanente.
- [ ] **5.4 TLS opcional**: `--tls-cert/--tls-key` (rustls), o documentar Caddy/Tailscale Serve como camino oficial y que `doctor` lo recomiende cuando se escucha fuera de loopback.
- [ ] **5.5 Límite de conexiones y timeouts de lectura/escritura** (semáforo en `serve`, `MAX_BODY` por ruta: pequeño salvo restore).
- [ ] **5.6 Registro de auditoría** de acciones destructivas (borrar, prune, exec, restore) en `~/.local/share/dockyard/audit.log`.
- [ ] **5.7 Sustituir `lock().unwrap()`** por `lock().unwrap_or_else(PoisonError::into_inner)` o un helper `lock()`; en release no abortar el panel por un pánico aislado.

## Fase 6 — Estructura y mantenibilidad

- [ ] **6.1 Separar módulos grandes**: `engine/{containers,images,volumes,exec,backup}.rs`, `app/{input,state,tabs}.rs`, `cli/{cmd_*}.rs`, `web/{server,routes,jobs}.rs`.
- [ ] **6.2 Capa de servicios compartida** (`ops.rs`): deploy, backup, restore, git, prune, usadas por CLI, TUI y web; las tres interfaces solo traducen entrada/salida.
- [ ] **6.3 Sustituir el servidor HTTP casero** por `hyper` (ya es dependencia transitiva de bollard) o `axum`, para keep-alive, límites y parseo correcto; mantener el binario pequeño (medir antes/después, hoy ~3 MB).
- [ ] **6.4 Errores tipados** en `engine` (`thiserror`) para distinguir «daemon caído», «no existe», «conflicto» y dar mensajes y códigos de salida coherentes en CLI.
- [ ] **6.5 Mover `web.html` y `web.css` a assets con comprobación** (el CSS ya es archivo aparte; añadir test que verifique que ambos se embeben).

## Fase 7 — Pruebas y CI

- [ ] **7.1 Dividir la integración** en pruebas pequeñas por función (deploy, logs, backup SQL, backup volumen, restore `--clean`, edit, compose, git) con prefijo `dytest-` y limpieza garantizada (`Drop`).
- [ ] **7.2 Script `scripts/test-docker.sh`** que corre clippy y pruebas dentro de `rust:latest` con el repo en solo lectura y volúmenes para `target`/caché (el mismo comando usado en este análisis). Nada se compila en el equipo.
- [ ] **7.3 CI**: fijar toolchain (`rust-toolchain.toml` + `rust-version`), añadir `cargo audit` y `cargo deny`, matriz Docker 25/27/29 para probar API < 1.52, y job opcional con Podman.
- [ ] **7.4 Pruebas del servidor web**: levantar `serve` en un puerto efímero y probar autenticación, `Host`/`Origin`, límites y tickets de descarga.
- [ ] **7.5 Prueba de humo del frontend** con Playwright (login por token, listar contenedores, abrir logs) contra un daemon de prueba.

## Fase 8 — Funciones

- [ ] **8.1 Respaldos programados nativos**: `dockyard backup --all --keep 7`, retención configurable y `dockyard backup verify` (restaura a un contenedor desechable y comprueba).
- [ ] **8.2 Releases binarios** (GitHub Actions: x86_64/aarch64 glibc y musl, macOS), corregir el placeholder `TU_USUARIO` del README e instalar con `cargo install --git` / script.
- [ ] **8.3 Notificaciones**: webhook/ntfy cuando un contenedor muere, un healthcheck falla o el disco supera un umbral.
- [ ] **8.4 Actualizaciones**: ver qué contenedores tienen imagen nueva (comparar digest) sin actualizar, y actualizar con rollback si el healthcheck falla.
- [ ] **8.5 Exportar/importar configuración** de un stack a Compose (`dockyard export NOMBRE`) para migrar o versionar.
- [ ] **8.6 i18n**: extraer cadenas a un módulo (`es` por defecto, `en` añadido) con la variable `LANG`.

## Orden recomendado

1. **5.1, 5.2, 5.7** (pequeños, cierran los riesgos más claros del panel web).
2. **7.2** (pruebas siempre en Docker) y **7.1**.
3. **5.5, 5.6, 6.3** (endurecer y reemplazar el servidor HTTP).
4. **6.1, 6.2** (refactor, protegido por las pruebas de 7.x).
5. **8.1, 8.2**, después el resto de la fase 8.

## Reglas de trabajo

- Todo lo que compile o ejecute se hace en Docker, nunca en el equipo.
- Un commit por ajuste en la rama actual; push solo cuando se pida.
