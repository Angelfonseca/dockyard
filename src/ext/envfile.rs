//! Archivos .env: lectura, escritura segura (0600) y detección de los que faltan.

use super::*;

pub(super) const ENV_EXAMPLES: [&str; 6] = [".env.example", ".env.sample", ".env.template", ".env.dist", "example.env", "env.example"];
pub(super) const MAX_ENV: usize = 64 * 1024;

pub(super) fn env_key_ok(k: &str) -> bool {
    let mut c = k.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_') && c.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// «CLAVE=valor» por línea (acepta `export`, comillas y comentarios). Error por número de línea: no se repite el contenido.
pub fn parse_env(text: &str) -> Result<Vec<(String, String)>> {
    if text.len() > MAX_ENV || text.contains('\0') {
        bail!("las variables son demasiado grandes o inválidas (máx. 64 KiB)");
    }
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        let l = l.strip_prefix("export ").unwrap_or(l).trim_start();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((k, v)) = l.split_once('=').filter(|(k, _)| env_key_ok(k.trim())) else {
            bail!("línea {} de las variables inválida: debe ser NOMBRE=valor", n + 1);
        };
        let v = v.trim();
        let v = if v.len() >= 2 && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\''))) { &v[1..v.len() - 1] } else { v };
        out.push((k.trim().to_string(), v.to_string()));
    }
    Ok(out)
}

/// Escribe un archivo de variables dentro de `ctx` (0600). Nunca fuera de la carpeta, ni a través de symlinks del repo.
pub(super) fn write_env(ctx: &Path, f: &EnvFile) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    f.check()?;
    let path = ctx.join(&f.name);
    let root = ctx.canonicalize()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(&root) {
            bail!("«{}» sale del repositorio", f.name);
        }
    }
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("«{}» es un enlace simbólico del repositorio; no se escribe", f.name);
    }
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    std::io::Write::write_all(&mut file, f.text.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Rutas que el compose declara en `env_file:` (lectura por líneas, sin YAML: cubre `env_file: x`, `[a, b]` y listas con «- x»).
pub(super) fn compose_env_files(compose: &str) -> Vec<String> {
    let clean = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'' || c == ',').to_string();
    let mut out: Vec<String> = Vec::new();
    let mut lines = compose.lines().peekable();
    while let Some(l) = lines.next() {
        let Some(rest) = l.trim().strip_prefix("env_file:") else { continue };
        let rest = rest.trim();
        if rest.is_empty() {
            let indent = l.len() - l.trim_start().len();
            while let Some(n) = lines.peek() {
                let t = n.trim();
                let i = n.len() - n.trim_start().len();
                if t.is_empty() || t.starts_with('#') { lines.next(); continue; }
                match t.strip_prefix("- ") {
                    Some(item) if i >= indent => { out.push(clean(item.strip_prefix("path:").unwrap_or(item))); lines.next(); }
                    _ => break,
                }
            }
        } else {
            out.extend(rest.trim_matches(|c| c == '[' || c == ']').split(',').map(clean));
        }
    }
    for p in out.iter_mut() {
        *p = p.trim_start_matches("./").to_string();
    }
    out.retain(|p| !p.is_empty() && !p.contains("${") && !p.contains("..") && !p.starts_with('/') && !p.contains(':'));
    out.sort();
    out.dedup();
    out
}

pub(super) fn read_small(p: &Path) -> Option<String> {
    use std::io::Read;
    let m = std::fs::symlink_metadata(p).ok()?;
    if !m.file_type().is_file() {
        return None; // ni symlinks ni carpetas
    }
    let mut buf = Vec::new();
    std::fs::File::open(p).ok()?.take(MAX_ENV as u64).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

