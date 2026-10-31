//! Respuestas HTTP, autenticación, sesiones temporales y el bucle de conexiones.

use super::*;

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

pub(super) fn parse(head: &str) -> Option<Req> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?;
    let (path, qs) = target.split_once('?').unwrap_or((target, ""));
    let query = qs
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (url_decode(k), url_decode(v))
        })
        .collect();
    let (mut token, mut length, mut chunked) = (String::new(), 0, false);
    let (mut host, mut origin) = (String::new(), None);
    for (k, v) in lines.filter_map(|l| l.split_once(':')) {
        let k = k.trim();
        if k.eq_ignore_ascii_case("x-dockyard-token") {
            token = v.trim().to_string();
        } else if k.eq_ignore_ascii_case("content-length") {
            length = v.trim().parse().unwrap_or(0);
        } else if k.eq_ignore_ascii_case("transfer-encoding") {
            chunked = true;
        } else if k.eq_ignore_ascii_case("host") {
            host = v.trim().to_string();
        } else if k.eq_ignore_ascii_case("origin") {
            origin = Some(v.trim().to_string());
        }
    }
    Some(Req { method, path: path.to_string(), query, token, host, origin, length, chunked })
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

pub(super) async fn send(sock: &mut TcpStream, r: Resp) -> Result<()> {
    let mut file = None;
    let len = match &r.file {
        Some(p) => {
            let f = tokio::fs::File::open(p).await?;
            let n = f.metadata().await?.len();
            file = Some(f);
            n
        }
        None => r.body.len() as u64,
    };
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {len}\r\nCache-Control: no-store\r\n\
         X-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\nCross-Origin-Opener-Policy: same-origin\r\n\
         Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=()\r\nConnection: close\r\n{}\r\n",
        r.status, r.ctype, r.extra
    );
    sock.write_all(head.as_bytes()).await?;
    match file {
        Some(mut f) => {
            tokio::io::copy(&mut f, sock).await?;
        }
        None => sock.write_all(&r.body).await?,
    }
    sock.shutdown().await.ok();
    Ok(())
}

pub(super) async fn handle(ctx: Arc<Ctx>, mut sock: TcpStream) -> Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                bail!("cerrada");
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok(p);
            }
            if buf.len() > 16 * 1024 {
                bail!("cabecera demasiado grande");
            }
        }
    })
    .await??;
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let Some(req) = parse(&head) else {
        return send(&mut sock, text("400 Bad Request", "petición inválida")).await;
    };

    if req.chunked {
        return send(&mut sock, text("400 Bad Request", "Transfer-Encoding no admitido")).await;
    }
    if !host_ok(&req.host, ctx.listen, &ctx.allow) || req.origin.as_deref().is_some_and(|o| !origin_ok(o, ctx.listen, &ctx.allow)) {
        return send(&mut sock, text("403 Forbidden", "host no permitido")).await;
    }
    // Públicas: la página (estática, sin datos) y la descarga con ticket. Todo lo demás exige el token.
    let public = req.method == "GET" && matches!(req.path.as_str(), "/" | "/app.css" | "/api/download");
    if !public {
        let ip = sock.peer_addr()?.ip();
        if blocked(&ctx, ip) {
            return send(&mut sock, text("429 Too Many Requests", "demasiados intentos; espera unos minutos")).await;
        }
        if !authed(&req, &ctx.token) {
            fail(&ctx, ip);
            let mut r = json_resp(json!({ "error": "sin acceso" }));
            r.status = "401 Unauthorized";
            return send(&mut sock, r).await;
        }
        forget(&ctx, ip);
        if req.method == "POST" && AUDITED.contains(&req.path.as_str()) {
            audit(ip, &req);
        }
    }

    if req.length > MAX_BODY {
        return send(&mut sock, text("413 Payload Too Large", "demasiado grande")).await;
    }
    let mut body = buf[head_end + 4..].to_vec();
    tokio::time::timeout(Duration::from_secs(30), async {
        while body.len() < req.length {
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        Ok::<_, std::io::Error>(())
    })
    .await??;

    let r = match route(&ctx, &req, &body).await {
        Ok(r) => r,
        Err(e) => {
            let mut r = json_resp(json!({ "error": format!("{e:#}") }));
            r.status = "400 Bad Request";
            r
        }
    };
    send(&mut sock, r).await
}

pub(super) fn text(status: &'static str, body: &str) -> Resp {
    Resp { status, ctype: "text/plain; charset=utf-8", body: body.as_bytes().to_vec(), extra: String::new(), file: None }
}

