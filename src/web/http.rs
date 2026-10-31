//! Respuestas HTTP, autenticación, sesiones temporales y el bucle de conexiones.

use super::*;
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited, StreamBody, combinators::BoxBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::convert::Infallible;
use tokio_util::io::ReaderStream;

// ───────────────────────────── HTTP ─────────────────────────────

pub(super) struct Req {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: Vec<(String, String)>,
    pub(super) token: String,
    pub(super) host: String,
    pub(super) origin: Option<String>,
    pub(super) length: usize,
    /// Transfer-Encoding: no se admite (solo Content-Length), evita ambigüedades con proxies.
    pub(super) chunked: bool,
}

impl Req {
    pub(super) fn q(&self, k: &str) -> Option<&str> {
        self.query.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    pub(super) fn need(&self, k: &str) -> Result<&str> {
        self.q(k).filter(|v| !v.is_empty()).ok_or_else(|| anyhow!("falta el parámetro «{k}»"))
    }
    pub(super) fn flag(&self, k: &str) -> bool {
        matches!(self.q(k), Some("1" | "true"))
    }
}

pub(super) struct Resp {
    pub(super) status: &'static str,
    pub(super) ctype: &'static str,
    pub(super) body: Vec<u8>,
    pub(super) extra: String,
    pub(super) file: Option<PathBuf>,
}

pub(super) fn json_resp(v: Value) -> Resp {
    Resp { status: "200 OK", ctype: "application/json", body: v.to_string().into_bytes(), extra: String::new(), file: None }
}

pub(super) fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pares `clave=valor` de la query string, decodificados.
pub(super) fn query_pairs(qs: Option<&str>) -> Vec<(String, String)> {
    qs.unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (url_decode(k), url_decode(v))
        })
        .collect()
}

/// Petición de la capa de transporte (hyper) en nuestro formato, con las cabeceras que usan las comprobaciones.
pub(super) fn from_parts(p: &hyper::http::request::Parts) -> Req {
    let header = |name: &str| p.headers.get(name).and_then(|v| v.to_str().ok()).map(|v| v.trim().to_string());
    Req {
        method: p.method.as_str().to_string(),
        path: p.uri.path().to_string(),
        query: query_pairs(p.uri.query()),
        token: header("x-dockyard-token").unwrap_or_default(),
        host: header("host").unwrap_or_default(),
        origin: header("origin"),
        length: header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0),
        chunked: p.headers.contains_key("transfer-encoding"),
    }
}

/// Nombre de un `Host` sin puerto (`[::1]:8787` → `::1`).
pub(super) fn host_name(h: &str) -> &str {
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    if h.matches(':').count() == 1 { h.split(':').next().unwrap_or("") } else { h }
}

/// Acepta `localhost` y las IP literales de la escucha (cualquiera si escucha en «todas»).
/// Un nombre DNS no se acepta nunca: así un dominio que se reasigna a esta IP (DNS rebinding) no entra.
/// Además, los nombres que el usuario declare con `--allow-host` (p. ej. el de Caddy o `tailscale serve`).
pub(super) fn host_ok(host: &str, listen: IpAddr, allow: &[String]) -> bool {
    let name = host_name(host.trim());
    if name.eq_ignore_ascii_case("localhost") || allow.iter().any(|a| a.eq_ignore_ascii_case(name)) {
        return true;
    }
    match name.parse::<IpAddr>() {
        Ok(ip) => listen.is_unspecified() || ip == listen,
        Err(_) => false,
    }
}

/// `Origin` = `esquema://host[:puerto]`; el «null» de las páginas aisladas se rechaza.
pub(super) fn origin_ok(origin: &str, listen: IpAddr, allow: &[String]) -> bool {
    origin.split_once("://").is_some_and(|(_, rest)| host_ok(rest.split('/').next().unwrap_or(""), listen, allow))
}

/// Ventana de bloqueo por IP: si hay un bloqueo vigente, la petición se rechaza sin mirar el token.
pub(super) fn blocked(ctx: &Ctx, ip: IpAddr) -> bool {
    let m = ctx.fails.lock().unwrap_or_else(PoisonError::into_inner);
    m.get(&ip).and_then(|f| f.until).is_some_and(|t| t > Instant::now())
}

pub(super) fn fail(ctx: &Ctx, ip: IpAddr) {
    let now = Instant::now();
    let mut m = ctx.fails.lock().unwrap_or_else(PoisonError::into_inner);
    if m.len() > 4096 {
        m.retain(|_, f| f.until.is_some_and(|t| t > now));
    }
    let f = m.entry(ip).or_insert(Fail { n: 0, until: None });
    f.n += 1;
    if f.n > FAIL_FREE {
        f.until = Some(now + lockout(f.n));
    }
    eprintln!("aviso: acceso rechazado desde {ip} ({} intentos)", f.n);
}

pub(super) fn forget(ctx: &Ctx, ip: IpAddr) {
    ctx.fails.lock().unwrap_or_else(PoisonError::into_inner).remove(&ip);
}

/// Rutas POST que cambian o borran algo. Cada una deja una línea en `audit.log`, sin el cuerpo (puede traer secretos).
pub(super) const AUDITED: &[&str] = &[
    "/api/act", "/api/down", "/api/exec", "/api/edit", "/api/deploy", "/api/update", "/api/pull", "/api/rmi",
    "/api/rmvol", "/api/backup", "/api/restore", "/api/rmbackup", "/api/prune", "/api/tunnel", "/api/tunnel/stop", "/api/git",
];

pub(super) fn audit(ip: IpAddr, req: &Req) {
    use std::os::unix::fs::OpenOptionsExt;
    let line = json!({
        "ts": now_secs(),
        "ip": ip.to_string(),
        "ruta": req.path,
        "params": req.query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
    });
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(dir.join("audit.log")) {
        let _ = std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes());
    }
}

pub(super) fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(super) fn authed(req: &Req, token: &str) -> bool {
    same(&req.token, token) || session_ok(&req.token)
}

// ── enlaces temporales: `dockyard web-link` crea un token que caduca; el permanente no cambia ──

pub(super) fn sessions_file() -> PathBuf {
    config_dir().join("web-sessions")
}

/// Líneas «<vencimiento-unix> <token>»: vale si el token coincide y no ha vencido.
pub(super) fn session_valid(text: &str, tok: &str, now: i64) -> bool {
    text.lines()
        .filter_map(|l| l.split_once(' '))
        .any(|(exp, t)| exp.parse::<i64>().is_ok_and(|e| e > now) && same(t, tok))
}

pub(super) fn session_ok(tok: &str) -> bool {
    !tok.is_empty() && std::fs::read_to_string(sessions_file()).is_ok_and(|t| session_valid(&t, tok, now_secs()))
}

/// Crea un enlace de `hours` horas y quita los vencidos. Devuelve el token y su vencimiento (unix).
pub(super) fn new_session(text: &str, tok: &str, expires: i64, now: i64) -> String {
    let mut out: String = text
        .lines()
        .filter(|l| l.split_once(' ').and_then(|(e, _)| e.parse::<i64>().ok()).is_some_and(|e| e > now))
        .map(|l| format!("{l}\n"))
        .collect();
    out.push_str(&format!("{expires} {tok}\n"));
    out
}

/// Imprime un enlace temporal para el panel que corre en `listen`.
pub fn print_link(listen: &str, hours: u64) -> Result<()> {
    anyhow::ensure!((1..=720).contains(&hours), "las horas deben estar entre 1 y 720");
    use std::os::unix::fs::OpenOptionsExt;
    let now = now_secs();
    let tok = rand_alnum(32);
    let expires = now + hours as i64 * 3600;
    let old = std::fs::read_to_string(sessions_file()).unwrap_or_default();
    std::fs::create_dir_all(config_dir())?;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(sessions_file())?;
    std::io::Write::write_all(&mut f, new_session(&old, &tok, expires, now).as_bytes())?;
    println!("Enlace temporal (vence en {hours} h): http://{listen}/#t={tok}");
    println!("Funciona mientras el panel esté corriendo en {listen}.");
    Ok(())
}

/// Cuerpo de respuesta: en memoria o en streaming (descargas), con el mismo tipo para ambos.
pub(super) type Body = BoxBody<Bytes, std::io::Error>;

/// Cabeceras de seguridad que van en toda respuesta.
const SECURITY: [(&str, &str); 6] = [
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cross-origin-resource-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
    ("permissions-policy", "camera=(), microphone=(), geolocation=(), payment=()"),
];

fn full(bytes: Vec<u8>) -> Body {
    BodyExt::boxed(Full::new(Bytes::from(bytes)).map_err(|never| match never {}))
}

/// Respuesta de hyper a partir de la nuestra. Una descarga se manda por trozos, sin cargar el archivo entero.
pub(super) async fn to_hyper(r: Resp) -> Response<Body> {
    let code = r.status.split(' ').next().and_then(|c| c.parse::<u16>().ok()).and_then(|c| StatusCode::from_u16(c).ok());
    let mut b = Response::builder().status(code.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)).header(CONTENT_TYPE, r.ctype);
    b = b.header("cache-control", "no-store");
    for (k, v) in SECURITY {
        b = b.header(k, v);
    }
    for line in r.extra.split("\r\n").filter(|l| !l.is_empty()) {
        if let Some((k, v)) = line.split_once(':') {
            b = b.header(k.trim(), v.trim());
        }
    }
    let body = match r.file {
        Some(p) => match tokio::fs::File::open(&p).await {
            Ok(f) => {
                let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
                b = b.header(CONTENT_LENGTH, len);
                BodyExt::boxed(StreamBody::new(TryStreamExt::map_ok(ReaderStream::new(f), Frame::data)))
            }
            Err(_) => {
                b = b.status(StatusCode::NOT_FOUND);
                full(b"no se pudo abrir el archivo".to_vec())
            }
        },
        None => {
            b = b.header(CONTENT_LENGTH, r.body.len());
            full(r.body)
        }
    };
    b.body(body).unwrap_or_else(|_| Response::new(full(Vec::new())))
}

/// Comprobaciones previas al cuerpo: transporte, Host/Origin, bloqueo, token, auditoría y tamaño.
/// Devuelve la respuesta de rechazo, o `None` si la petición puede seguir.
fn admit(ctx: &Ctx, ip: IpAddr, req: &Req) -> Option<Resp> {
    if req.chunked {
        return Some(text("400 Bad Request", "Transfer-Encoding no admitido"));
    }
    if !host_ok(&req.host, ctx.listen, &ctx.allow) || req.origin.as_deref().is_some_and(|o| !origin_ok(o, ctx.listen, &ctx.allow)) {
        return Some(text("403 Forbidden", "host no permitido"));
    }
    // Públicas: la página (estática, sin datos) y la descarga con ticket. Todo lo demás exige el token.
    let public = req.method == "GET" && matches!(req.path.as_str(), "/" | "/app.css" | "/api/download");
    if !public {
        if blocked(ctx, ip) {
            return Some(text("429 Too Many Requests", "demasiados intentos; espera unos minutos"));
        }
        if !authed(req, &ctx.token) {
            fail(ctx, ip);
            let mut r = json_resp(json!({ "error": "sin acceso" }));
            r.status = "401 Unauthorized";
            return Some(r);
        }
        forget(ctx, ip);
        if req.method == "POST" && AUDITED.contains(&req.path.as_str()) {
            audit(ip, req);
        }
    }
    if req.length > MAX_BODY {
        return Some(text("413 Payload Too Large", "demasiado grande"));
    }
    None
}

/// Lee el cuerpo con límite de tamaño y tiempo.
async fn read_body(body: Incoming) -> std::result::Result<Vec<u8>, Resp> {
    match tokio::time::timeout(Duration::from_secs(30), Limited::new(body, MAX_BODY).collect()).await {
        Ok(Ok(c)) => Ok(c.to_bytes().to_vec()),
        Ok(Err(e)) if e.downcast_ref::<LengthLimitError>().is_some() => Err(text("413 Payload Too Large", "demasiado grande")),
        Ok(Err(_)) => Err(text("400 Bad Request", "cuerpo incompleto")),
        Err(_) => Err(text("408 Request Timeout", "tiempo agotado leyendo el cuerpo")),
    }
}

async fn serve_one(ctx: Arc<Ctx>, ip: IpAddr, r: Request<Incoming>) -> Response<Body> {
    let (parts, body) = r.into_parts();
    let req = from_parts(&parts);
    if let Some(rej) = admit(&ctx, ip, &req) {
        return to_hyper(rej).await;
    }
    let bytes = match read_body(body).await {
        Ok(b) => b,
        Err(rej) => return to_hyper(rej).await,
    };
    let r = match route(&ctx, &req, &bytes).await {
        Ok(r) => r,
        Err(e) => {
            let mut r = json_resp(json!({ "error": format!("{e:#}") }));
            r.status = "400 Bad Request";
            r
        }
    };
    to_hyper(r).await
}

/// Una conexión HTTP/1 con keep-alive. Hyper valida el protocolo; el tiempo de cabeceras lo limita aquí.
pub(super) async fn handle(ctx: Arc<Ctx>, sock: TcpStream) -> Result<()> {
    let ip = sock.peer_addr()?.ip();
    let svc = service_fn(move |r: Request<Incoming>| {
        let ctx = ctx.clone();
        async move { Ok::<_, Infallible>(serve_one(ctx, ip, r).await) }
    });
    http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10))
        .serve_connection(TokioIo::new(sock), svc)
        .await?;
    Ok(())
}

pub(super) fn text(status: &'static str, body: &str) -> Resp {
    Resp { status, ctype: "text/plain; charset=utf-8", body: body.as_bytes().to_vec(), extra: String::new(), file: None }
}

