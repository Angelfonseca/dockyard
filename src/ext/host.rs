//! Shell y comandos de la máquina anfitriona, y edición en el editor del usuario.

use super::*;

// ───────────────────────────── Host y editor ─────────────────────────────

/// Shell interactiva de «la máquina»: la local, o la del servidor si el daemon es remoto por SSH.
pub fn host_shell(engine: &Engine) -> std::process::Command {
    if let Some((target, port)) = engine.ssh_target() {
        let mut c = std::process::Command::new("ssh");
        c.arg("-t");
        if let Some(p) = port {
            c.arg("-p").arg(p.to_string());
        }
        c.arg(target);
        return c;
    }
    let sh = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let mut c = std::process::Command::new(sh);
    c.env("DOCKER_HOST", &engine.host);
    c
}

/// Comando de una sola vez en la máquina (local o el servidor SSH).
pub fn host_command(engine: &Engine, line: &str) -> tokio::process::Command {
    if let Some((target, port)) = engine.ssh_target() {
        let mut c = tokio::process::Command::new("ssh");
        c.args(["-o", "BatchMode=yes"]);
        if let Some(p) = port {
            c.arg("-p").arg(p.to_string());
        }
        c.arg(target).arg("--").arg(line);
        return c;
    }
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(line).env("DOCKER_HOST", &engine.host);
    c
}

/// Abre `content` en el editor (bloquea hasta cerrarlo). Some(nuevo) si cambió.
pub fn edit_in_editor(content: &[u8], file_name: &str) -> Result<Option<Vec<u8>>> {
    let Some(ed) = editor() else { bail!("no hay editor: define $EDITOR o instala nano") };
    let safe: String = file_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
    let tmp = std::env::temp_dir().join(format!("dockyard-{}-{safe}", rand_alnum(6)));
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(content)?;
    }
    let st = std::process::Command::new(&ed[0]).args(&ed[1..]).arg(&tmp).status();
    let new = std::fs::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let st = st.with_context(|| format!("no se pudo abrir el editor «{}»", ed[0]))?;
    if !st.success() {
        bail!("el editor terminó con error; no se guardó nada");
    }
    let new = new?;
    Ok(if new == content { None } else { Some(new) })
}

