//! Shell interactiva dentro de un contenedor: exec con TTY por la API (no necesita el CLI de docker,
//! funciona igual con un daemon remoto). La terminal pasa a modo raw y se redimensiona con SIGWINCH.

use crate::engine::Engine;
use anyhow::{Result, bail};
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::query_parameters::ResizeExecOptionsBuilder;
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Write;
use tokio::io::AsyncWriteExt;

/// bash si existe, si no sh.
pub fn default_shell() -> Vec<String> {
    vec!["sh".into(), "-c".into(), "if [ -x /bin/bash ]; then exec /bin/bash; else exec /bin/sh; fi".into()]
}

/// Restaura el modo normal de la terminal aunque algo falle a medio camino.
struct RawGuard;
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Ejecuta `cmd` con TTY conectado a esta terminal. Devuelve el código de salida.
pub async fn interactive(engine: &Engine, id: &str, cmd: Vec<String>) -> Result<Option<i64>> {
    let size = || crossterm::terminal::size().ok().filter(|(w, h)| *w > 0 && *h > 0);
    let (w, h) = size().unwrap_or((80, 24));
    let exec = engine.create_tty_exec(id, cmd, true, Some((w, h))).await?;
    let started = engine.d.start_exec(&exec, Some(StartExecOptions { detach: false, tty: true, output_capacity: None })).await?;
    let StartExecResults::Attached { mut output, mut input } = started else { bail!("el daemon no adjuntó la terminal") };
    let resize = |w: u16, h: u16| {
        let d = engine.d.clone();
        let exec = exec.clone();
        async move {
            let _ = d.resize_exec(&exec, ResizeExecOptionsBuilder::default().w(w as i32).h(h as i32).build()).await;
        }
    };
    resize(w, h).await;

    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let stop2 = stop.clone();
    // Lector de teclado: poll con espera corta para poder terminar sin «robar» la siguiente tecla
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while !stop2.load(Ordering::SeqCst) {
            let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, 100) };
            if r <= 0 || pfd.revents & libc::POLLIN == 0 {
                continue;
            }
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 || tx.blocking_send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut out = std::io::stdout();
    let mut err: Option<anyhow::Error> = None;
    loop {
        tokio::select! {
            chunk = output.next() => match chunk {
                Some(Ok(c)) => {
                    if out.write_all(c.as_ref()).is_err() { break; }
                    let _ = out.flush();
                }
                Some(Err(e)) => { err = Some(e.into()); break; }
                None => break,
            },
            Some(data) = rx.recv() => {
                if input.write_all(&data).await.is_err() { break; }
                let _ = input.flush().await;
            }
            _ = winch.recv() => {
                if let Some((w, h)) = size() {
                    resize(w, h).await;
                }
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    let _ = reader.join();
    drop(_guard);
    if let Some(e) = err {
        return Err(e);
    }
    Ok(engine.exec_code(&exec).await)
}
