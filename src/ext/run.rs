//! Ejecución de procesos con salida por líneas y entorno de Docker.

use super::*;

// ───────────────────────────── Ejecución con salida por líneas ─────────────────────────────

/// Prepara un comando que hable con el mismo daemon que dockyard.
pub(super) fn docker_env(cmd: &mut tokio::process::Command, engine: &Engine) {
    cmd.env("DOCKER_HOST", &engine.host).env_remove("DOCKER_CONTEXT");
}

/// Ejecuta y manda cada línea de stdout/stderr a `out`. Error si termina con código ≠ 0.
pub async fn run_lines(mut cmd: tokio::process::Command, label: &str, out: Out<'_>) -> Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().with_context(|| format!("no se pudo ejecutar {label}"))?;
    let mut so = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("sin stdout"))?).lines();
    let mut se = BufReader::new(child.stderr.take().ok_or_else(|| anyhow!("sin stderr"))?).lines();
    let (mut o_done, mut e_done) = (false, false);
    let mut last_err = Vec::new();
    while !(o_done && e_done) {
        tokio::select! {
            l = so.next_line(), if !o_done => match l {
                Ok(Some(l)) => out(strip_ansi(&l)),
                _ => o_done = true,
            },
            l = se.next_line(), if !e_done => match l {
                Ok(Some(l)) => {
                    let l = strip_ansi(&l);
                    last_err.push(l.clone());
                    if last_err.len() > 6 {
                        last_err.remove(0);
                    }
                    out(l);
                }
                _ => e_done = true,
            },
        }
    }
    let st = child.wait().await?;
    if !st.success() {
        bail!("{label} terminó con código {}: {}", st.code().unwrap_or(-1), last_err.join(" · "));
    }
    Ok(())
}

