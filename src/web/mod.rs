//! Panel web (`dockyard web`): una página con vistas de panel, contenedores, tienda de plantillas, imágenes,
//! volúmenes, respaldos y espacio. Servidor HTTP propio sobre tokio (sin dependencias nuevas). El acceso es con un
//! token que la página guarda en `localStorage` y manda en la cabecera `X-Dockyard-Token`. No hay cookie a propósito:
//! las cookies no distinguen puertos, y cualquier contenedor publicado en este mismo servidor la habría recibido.
//! Una cabecera propia tampoco la manda otra página sin permiso de CORS, que aquí nunca se concede.
//! Las tareas largas (instalar, actualizar, respaldar…) corren como «trabajos» que la página consulta.

use crate::app::{can_bind, suggest_name};
use crate::catalog::{Catalog, Plan, PortSpec};
use crate::collector::{self, Shared};
use crate::config::Config;
use crate::engine::{Act, Engine, UpdateOutcome, timed};
use crate::ext;
use crate::tunnel;
use crate::model::*;
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PAGE: &str = include_str!("../web.html");
/// Tailwind 3.4.17 compilado una vez (sin script de terceros en el panel). Regenerar tras cambiar clases en web.html:
/// `npx tailwindcss@3.4.17 -i in.css -o src/web.css --minify` con `@tailwind base; @tailwind components; @tailwind utilities;` y content = src/web.html
const CSS: &str = include_str!("../web.css");
/// Ninguna ruta sube archivos: todas reciben JSON pequeño (los respaldos se piden por nombre), así que 256 KB sobran.
const MAX_BODY: usize = 256 << 10;
const MAX_CONNS: usize = 128;
const TICKET_SECS: u64 = 60;

#[derive(Default)]
struct State {
    snap: Option<Box<Snapshot>>,
    error: Option<String>,
    usage: Option<Box<DiskUsage>>,
    usage_asked: Option<Instant>,
}

#[derive(Serialize)]
struct Job {
    title: String,
    lines: Vec<String>,
    done: bool,
    ok: bool,
    result: Value,
}

struct Ctx {
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    state: Mutex<State>,
    token: String,
    catalog: Catalog,
    cfg: Config,
    /// IP de escucha si no es local ni «todas» (p. ej. la de Tailscale): se ofrece para publicar puertos.
    bind_ip: Option<String>,
    jobs: Mutex<(u64, BTreeMap<u64, Job>)>,
    /// Descargas autorizadas: ticket de un solo uso (60 s) → archivo de respaldo.
    tickets: Mutex<HashMap<String, (String, Instant)>>,
    /// Dirección de escucha: solo se aceptan `Host`/`Origin` que la nombren (o `localhost`).
    listen: IpAddr,
    /// Nombres de `Host` adicionales aceptados (`--allow-host`), para proxies con dominio.
    allow: Vec<String>,
    /// Tokens inválidos por IP, para frenar la adivinanza de la contraseña del panel.
    fails: Mutex<HashMap<IpAddr, Fail>>,
}

/// Intentos fallidos de una IP. Tras `FAIL_FREE` se bloquea hasta `until`, con espera creciente.
struct Fail {
    n: u32,
    until: Option<Instant>,
}

const FAIL_FREE: u32 = 4;

/// Espera tras el intento número `n` (> `FAIL_FREE`): 30 s, 60 s, 120 s… con tope de 15 min.
fn lockout(n: u32) -> Duration {
    Duration::from_secs((30u64 << (n - FAIL_FREE - 1).min(5)).min(900))
}

/// Token guardado en `~/.config/dockyard/web-token` (0600) para que la URL siga valiendo tras reiniciar.
/// Con `rotate`, se genera uno nuevo: el anterior deja de valer al reiniciar el panel.
fn load_token(rotate: bool) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = config_dir().join("web-token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        if !rotate && t.trim().len() >= 24 {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            return Ok(t.trim().to_string());
        }
    }
    std::fs::create_dir_all(config_dir())?;
    let t = rand_alnum(32);
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    std::io::Write::write_all(&mut f, t.as_bytes())?;
    Ok(t)
}

pub async fn serve(engine: Engine, listen: &str, cfg: Config, catalog: Catalog, rotate: bool, allow: Vec<String>) -> Result<()> {
    let token = load_token(rotate)?;
    if rotate {
        println!("Token rotado: el anterior ya no sirve. Abre la nueva URL en cada navegador.");
    }
    let listener = TcpListener::bind(listen).await.with_context(|| format!("no se pudo escuchar en {listen}"))?;
    let addr = listener.local_addr()?;
    if addr.ip().is_unspecified() {
        eprintln!("aviso: {addr} acepta conexiones de cualquier red; mejor usa la IP de Tailscale o 127.0.0.1");
    }
    if !addr.ip().is_loopback() {
        // sin TLS propio: el tráfico por Tailscale va cifrado (WireGuard); otra red necesita HTTPS delante (README)
        eprintln!("aviso: el panel va sin TLS en {addr}. Úsalo por Tailscale o detrás de un proxy con HTTPS (ver README).");
    }
    // el token va en el fragmento (#): el navegador nunca lo envía al servidor, ni a proxies, ni en Referer
    println!("Panel web: http://{addr}/#t={token}");
    println!("(abre esa URL una vez; el navegador guarda el acceso. Ctrl+C para detener)");

    let bind_ip = (!addr.ip().is_unspecified() && !addr.ip().is_loopback()).then(|| addr.ip().to_string());
    let engine = Arc::new(engine);
    let shared = Shared::new(cfg.refresh_ms());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
    tokio::spawn(collector::run(engine.clone(), shared.clone(), tx.clone()));
    tokio::spawn(collector::run_usage(engine.clone(), shared.clone(), tx));
    engine.watch_events(shared.notify.clone());

    let ctx = Arc::new(Ctx {
        engine,
        shared,
        state: Mutex::new(State::default()),
        token,
        catalog,
        cfg,
        bind_ip,
        jobs: Mutex::new((0, BTreeMap::new())),
        tickets: Mutex::new(HashMap::new()),
        listen: addr.ip(),
        allow,
        fails: Mutex::new(HashMap::new()),
    });
    let c = ctx.clone();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let mut st = c.state.lock().unwrap_or_else(PoisonError::into_inner);
            match m {
                Msg::Snapshot(s) => {
                    st.snap = Some(s);
                    st.error = None;
                }
                Msg::Disconnected(e) => st.error = Some(e),
                Msg::Usage(u) => {
                    st.usage = Some(u);
                    // nadie mira el uso de disco: dejar de medir
                    if st.usage_asked.is_none_or(|t| t.elapsed() > Duration::from_secs(90)) {
                        c.shared.want_usage.store(false, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }
    });

    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNS));
    loop {
        let (sock, _) = listener.accept().await?;
        // sin cupo se cierra la conexión: un cliente lento o malicioso no agota los descriptores
        let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = handle(ctx, sock).await;
            drop(permit);
        });
    }
}


mod http;
mod jobs;
mod routes;
#[cfg(test)]
mod tests;

use http::*;
use jobs::*;
use routes::*;
pub use http::print_link;
