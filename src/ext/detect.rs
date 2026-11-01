//! Detección de herramientas del sistema: `docker compose`, editor de texto.

use super::*;

// ───────────────────────────── Detección ─────────────────────────────

/// Programa para Compose: `docker compose` (v2) o `docker-compose` (v1). None si no hay ninguno.
pub fn compose_cmd() -> Option<Vec<String>> {
    static C: OnceLock<Option<Vec<String>>> = OnceLock::new();
    C.get_or_init(|| {
        if which("docker").is_some() {
            let ok = std::process::Command::new("docker")
                .args(["compose", "version"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Some(vec!["docker".into(), "compose".into()]);
            }
        }
        which("docker-compose").map(|_| vec!["docker-compose".into()])
    })
    .clone()
}

pub(super) fn need(prog: &str, what: &str) -> Result<()> {
    if which(prog).is_none() {
        bail!("{what} necesita «{prog}», que no está instalado (en Arch: sudo pacman -S {prog})");
    }
    Ok(())
}

/// Editor para archivos: $VISUAL, $EDITOR, o el primero de nano/vim/vi.
pub fn editor() -> Option<Vec<String>> {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            let w = split_words(&v);
            if !w.is_empty() {
                return Some(w);
            }
        }
    }
    ["nano", "vim", "vi"].iter().find(|p| which(p).is_some()).map(|p| vec![p.to_string()])
}

