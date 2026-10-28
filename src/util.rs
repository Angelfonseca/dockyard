//! Utilidades pequeñas sin dependencias extra: formato, parseo, aleatorios, fechas UTC.

use anyhow::{Result, bail};
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else if v >= 100.0 {
        format!("{:.0}{}", v, U[i])
    } else if v >= 10.0 {
        format!("{:.1}{}", v, U[i])
    } else {
        format!("{:.2}{}", v, U[i])
    }
}

pub fn human_rate(bps: f64) -> String {
    format!("{}/s", human_bytes(bps.max(0.0) as u64))
}

/// "3d", "5h", "12m", "40s"
pub fn human_age(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 86_400 * 2 {
        format!("{}d", s / 86_400)
    } else if s >= 3_600 {
        format!("{}h", s / 3_600)
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

pub fn human_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Texto recortado a `n` caracteres con elipsis.
pub fn truncate(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else if n == 1 {
        "…".into()
    } else {
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}

pub fn short_id(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    &id[..id.len().min(12)]
}

/// Interpreta "512m", "1g", "1.5g", "2048k", "100000" (bytes). Vacío o "0" => sin límite.
pub fn parse_mem(s: &str) -> Result<Option<i64>> {
    let t = s.trim().to_lowercase();
    if t.is_empty() || t == "0" || t == "-" || t == "sin limite" || t == "sin límite" {
        return Ok(None);
    }
    let t = t.trim_end_matches('b');
    let (num, mult) = match t.chars().last() {
        Some('k') => (&t[..t.len() - 1], 1024f64),
        Some('m') => (&t[..t.len() - 1], 1024f64 * 1024.0),
        Some('g') => (&t[..t.len() - 1], 1024f64 * 1024.0 * 1024.0),
        Some('t') => (&t[..t.len() - 1], 1024f64 * 1024.0 * 1024.0 * 1024.0),
        _ => (t, 1f64),
    };
    let n: f64 = match num.trim().parse() {
        Ok(n) => n,
        Err(_) => bail!("límite de memoria inválido: «{s}» (ej. 512m, 1g)"),
    };
    if n < 0.0 {
        bail!("el límite de memoria no puede ser negativo");
    }
    let bytes = (n * mult) as i64;
    if bytes < 6 * 1024 * 1024 {
        bail!("Docker exige al menos 6M de memoria");
    }
    Ok(Some(bytes))
}

/// Límite de CPU en núcleos ("1.5", "0.25") → NanoCpus de Docker. Vacío o "0" => sin límite.
pub fn parse_cpus(s: &str) -> Result<Option<i64>> {
    let t = s.trim().replace(',', ".");
    if t.is_empty() || t == "0" || t == "-" {
        return Ok(None);
    }
    let n: f64 = match t.parse() {
        Ok(n) => n,
        Err(_) => bail!("límite de CPU inválido: «{s}» (ej. 0.5, 2)"),
    };
    if !(0.01..=1024.0).contains(&n) {
        bail!("el límite de CPU debe estar entre 0.01 y 1024 núcleos");
    }
    Ok(Some((n * 1e9).round() as i64))
}

/// Inverso de `parse_cpus`.
pub fn cpus_to_text(nano: i64) -> String {
    if nano <= 0 {
        return String::new();
    }
    let v = nano as f64 / 1e9;
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Inverso de `parse_mem`, para mostrar en el formulario.
pub fn mem_to_text(bytes: i64) -> String {
    if bytes <= 0 {
        return String::new();
    }
    const G: i64 = 1024 * 1024 * 1024;
    const M: i64 = 1024 * 1024;
    if bytes % G == 0 {
        format!("{}g", bytes / G)
    } else if bytes % M == 0 {
        format!("{}m", bytes / M)
    } else {
        format!("{}m", (bytes as f64 / M as f64).round() as i64)
    }
}

/// Cadena aleatoria alfanumérica (de /dev/urandom; con respaldo por reloj).
pub fn rand_alnum(n: usize) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(n);
    let mut buf = vec![0u8; n * 3 + 16];
    let got = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !got {
        // Respaldo: LCG sembrado con el reloj. Solo se usa si /dev/urandom no existe.
        let mut x = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            | 1;
        for b in buf.iter_mut() {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (x >> 33) as u8;
        }
    }
    for b in buf {
        if out.len() == n {
            break;
        }
        // 248 = 62 * 4: descartar el resto evita sesgo de módulo
        if b < 248 {
            out.push(A[(b % 62) as usize] as char);
        }
    }
    while out.len() < n {
        out.push('x');
    }
    out
}

/// Fecha UTC "20261026-182233" (algoritmo civil de Howard Hinnant).
pub fn utc_stamp() -> String {
    let secs = now_secs();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Quita secuencias ANSI (colores, cursor) de una línea de log.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            match it.peek() {
                Some('[') => {
                    it.next();
                    for n in it.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(n) = it.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            it.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    it.next();
                }
                None => {}
            }
        } else if c == '\r' {
            // retorno de carro (barras de progreso): descartar
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Separa una línea en argumentos respetando comillas simples y dobles.
pub fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Inverso de `split_words`: une argumentos, entrecomillando los que tienen espacios.
pub fn join_words(v: &[String]) -> String {
    v.iter()
        .map(|a| {
            if a.is_empty() || a.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'') {
                if a.contains('"') {
                    format!("'{a}'")
                } else {
                    format!("\"{a}\"")
                }
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(x).join("dockyard");
    }
    home_dir().join(".config").join("dockyard")
}

pub fn backups_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("DOCKYARD_BACKUPS") {
        return PathBuf::from(x);
    }
    home_dir().join("dockyard-backups")
}

/// Datos propios (repositorios clonados): $XDG_DATA_HOME/dockyard o ~/.local/share/dockyard.
pub fn data_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(x).join("dockyard");
    }
    home_dir().join(".local").join("share").join("dockyard")
}

/// Respaldo más reciente «<nombre>-AAAAMMDD-HHMMSS.<ext>» en `dir` (no confunde «pg» con «pg-db»).
pub fn latest_backup(dir: &std::path::Path, name: &str, ext: &str) -> Option<PathBuf> {
    let mut best: Option<(String, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let f = e.file_name().to_string_lossy().to_string();
        let Some(rest) = f.strip_prefix(&format!("{name}-")) else { continue };
        let Some(stamp) = rest.strip_suffix(&format!(".{ext}")) else { continue };
        let b = stamp.as_bytes();
        let ok = b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit());
        if ok && best.as_ref().map(|(s, _)| stamp > s.as_str()).unwrap_or(true) {
            best = Some((stamp.to_string(), e.path()));
        }
    }
    best.map(|(_, p)| p)
}

/// ¿Existe este programa en el PATH?
pub fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

/// Zona horaria del sistema (ej. "America/Mexico_City") leyendo /etc/localtime o /etc/timezone.
pub fn detect_tz() -> Option<String> {
    if let Ok(p) = std::fs::read_link("/etc/localtime") {
        let s = p.to_string_lossy().to_string();
        if let Some(i) = s.find("zoneinfo/") {
            let tz = &s[i + "zoneinfo/".len()..];
            if !tz.is_empty() {
                return Some(tz.to_string());
            }
        }
    }
    std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Nombre del repositorio de una imagen sin registro ni etiqueta:
/// "ghcr.io/topi314/spotify-tokener:master" -> "topi314/spotify-tokener"; "postgres:17" -> "postgres".
pub fn image_repo(image: &str) -> String {
    let no_digest = image.split('@').next().unwrap_or(image);
    let (path, _tag) = split_image_tag(no_digest);
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 1 && (parts[0].contains('.') || parts[0].contains(':') || parts[0] == "localhost") {
        parts.remove(0);
    }
    let mut s = parts.join("/");
    if let Some(r) = s.strip_prefix("library/") {
        s = r.to_string();
    }
    s
}

/// Separa "repo:tag" respetando puertos de registro ("host:5000/img:tag").
pub fn split_image_tag(image: &str) -> (&str, Option<&str>) {
    let last_slash = image.rfind('/').map(|i| i + 1).unwrap_or(0);
    match image[last_slash..].rfind(':') {
        Some(i) => {
            let idx = last_slash + i;
            (&image[..idx], Some(&image[idx + 1..]))
        }
        None => (image, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_roundtrip() {
        assert_eq!(parse_mem("512m").unwrap(), Some(512 * 1024 * 1024));
        assert_eq!(parse_mem("1g").unwrap(), Some(1024 * 1024 * 1024));
        assert_eq!(parse_mem("1.5G").unwrap(), Some(1536 * 1024 * 1024));
        assert_eq!(parse_mem("").unwrap(), None);
        assert_eq!(parse_mem("0").unwrap(), None);
        assert!(parse_mem("abc").is_err());
        assert!(parse_mem("1m").is_err());
        assert_eq!(mem_to_text(512 * 1024 * 1024), "512m");
        assert_eq!(mem_to_text(2 * 1024 * 1024 * 1024), "2g");
        assert_eq!(mem_to_text(0), "");
    }

    #[test]
    fn cpus_roundtrip() {
        assert_eq!(parse_cpus("1.5").unwrap(), Some(1_500_000_000));
        assert_eq!(parse_cpus("0,5").unwrap(), Some(500_000_000));
        assert_eq!(parse_cpus("").unwrap(), None);
        assert!(parse_cpus("0.001").is_err());
        assert!(parse_cpus("dos").is_err());
        assert_eq!(cpus_to_text(1_500_000_000), "1.5");
        assert_eq!(cpus_to_text(2_000_000_000), "2");
        assert_eq!(cpus_to_text(0), "");
    }

    #[test]
    fn latest_backup_picks_newest_exact_name() {
        let d = std::env::temp_dir().join(format!("dy-bk-{}", rand_alnum(8)));
        std::fs::create_dir_all(&d).unwrap();
        for f in ["pg-20260101-000000.sql", "pg-20260301-120000.sql", "pg-db-20261231-000000.sql", "pg-20270101-000000.sql.tmp", "pg-x.sql"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        assert_eq!(latest_backup(&d, "pg", "sql").unwrap().file_name().unwrap(), "pg-20260301-120000.sql");
        assert_eq!(latest_backup(&d, "pg-db", "sql").unwrap().file_name().unwrap(), "pg-db-20261231-000000.sql");
        assert!(latest_backup(&d, "otro", "sql").is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn words() {
        assert_eq!(split_words(r#"sh -c "echo hola mundo""#), vec!["sh", "-c", "echo hola mundo"]);
        assert_eq!(split_words("a  b   c"), vec!["a", "b", "c"]);
        assert_eq!(split_words("''"), vec![""]);
        let v = vec!["redis-server".to_string(), "--requirepass".into(), "a b".into()];
        assert_eq!(split_words(&join_words(&v)), v);
    }

    #[test]
    fn images() {
        assert_eq!(image_repo("postgres:17-alpine"), "postgres");
        assert_eq!(image_repo("ghcr.io/topi314/spotify-tokener:master"), "topi314/spotify-tokener");
        assert_eq!(image_repo("docker.io/library/redis"), "redis");
        assert_eq!(image_repo("localhost:5000/foo/bar:1"), "foo/bar");
        assert_eq!(split_image_tag("host:5000/img"), ("host:5000/img", None));
        assert_eq!(split_image_tag("host:5000/img:2"), ("host:5000/img", Some("2")));
    }

    #[test]
    fn ansi_and_rand() {
        assert_eq!(strip_ansi("\u{1b}[31mrojo\u{1b}[0m ok\r"), "rojo ok");
        let r = rand_alnum(24);
        assert_eq!(r.len(), 24);
        assert!(r.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(rand_alnum(24), rand_alnum(24));
    }

    #[test]
    fn stamp_shape() {
        let s = utc_stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
    }

    #[test]
    fn bytes() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1536), "1.50K");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.00G");
    }
}
