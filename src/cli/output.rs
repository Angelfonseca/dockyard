//! Salida de la CLI: tablas, confirmaciones, barras de progreso y búsqueda de contenedores.

use super::*;

pub(super) fn table(rows: &[Vec<String>]) {
    if rows.is_empty() {
        return;
    }
    let cols = rows[0].len();
    let mut w = vec![0usize; cols];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == cols {
                line.push_str(c);
            } else {
                line.push_str(&format!("{:<width$}  ", c, width = w[i]));
            }
        }
        println!("{}", line.trim_end());
    }
}

pub(super) fn confirm(msg: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    eprint!("{msg} [s/N] ");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(matches!(s.trim().to_lowercase().as_str(), "s" | "si" | "sí" | "y" | "yes"))
}

pub(super) fn progress_printer() -> impl FnMut(String) + Send {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let mut last = String::new();
    move |s: String| {
        // sin terminal (scripts, logs) la barra de progreso solo ensucia
        if tty && s != last {
            eprint!("\r\x1b[K{s}");
            std::io::stderr().flush().ok();
            last = s;
        }
    }
}

pub(super) fn clear_progress() {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K");
    }
}

pub(super) fn line_printer() -> impl FnMut(String) + Send {
    |s: String| eprintln!("{s}")
}

pub(super) async fn find(engine: &Engine, name: &str) -> Result<CRow> {
    let all = engine.list_containers().await?;
    if let Some(c) = all.iter().find(|c| c.name == name) {
        return Ok(c.clone());
    }
    let matches: Vec<&CRow> = all.iter().filter(|c| c.id.starts_with(name) || c.name.starts_with(name)).collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => bail!("no existe el contenedor «{name}»"),
        _ => bail!("«{name}» es ambiguo: {}", matches.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

pub(super) async fn volume_exists(engine: &Engine, name: &str) -> Result<bool> {
    Ok(engine.list_volumes(&[]).await?.iter().any(|v| v.name == name))
}

pub(super) fn usage_row(name: &str, u: &Usage) -> Vec<String> {
    if !u.known {
        return vec![name.into(), "-".into(), "-".into(), "n/d".into(), "n/d".into()];
    }
    vec![name.into(), u.count.to_string(), u.active.to_string(), human_bytes(u.size), human_bytes(u.reclaimable)]
}
