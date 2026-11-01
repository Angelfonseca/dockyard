//! Ejecución de comandos dentro de un contenedor (con salida, con TTY y desde archivo).

use super::*;

impl Engine {
    // ───────────────────────────── Exec ─────────────────────────────

    /// Ejecuta un comando (sin TTY) y devuelve (salida combinada, código de salida).
    pub async fn exec_collect(&self, id: &str, cmd: Vec<String>, timeout: Duration) -> Result<(String, Option<i64>)> {
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd),
                    ..Default::default()
                },
            )
            .await?
            .id;
        let mut out = String::new();
        let run = async {
            if let StartExecResults::Attached { mut output, .. } = self.d.start_exec(&exec, None).await? {
                while let Some(chunk) = output.next().await {
                    out.push_str(&chunk?.to_string());
                    if out.len() > 2_000_000 {
                        out.push_str("\n… salida truncada …");
                        break;
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(timeout, run).await {
            Ok(r) => r?,
            Err(_) => out.push_str(&format!("\n… tiempo agotado ({}s) …", timeout.as_secs())),
        }
        let code = self.d.inspect_exec(&exec).await.ok().and_then(|i| i.exit_code);
        Ok((out, code))
    }

    /// Ejecuta `sh -c <script>` y escribe stdout en un archivo. Devuelve (bytes, código, cola de stderr).
    pub async fn exec_to_file(&self, id: &str, script: &str, path: &Path) -> Result<(u64, Option<i64>, String)> {
        use std::io::Write;
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    ..Default::default()
                },
            )
            .await?
            .id;
        let mut file = std::fs::File::create(path).with_context(|| format!("no se pudo crear {}", path.display()))?;
        let mut written = 0u64;
        let mut errtxt = String::new();
        if let StartExecResults::Attached { mut output, .. } = self.d.start_exec(&exec, None).await? {
            while let Some(chunk) = output.next().await {
                match chunk? {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        file.write_all(&message)?;
                        written += message.len() as u64;
                    }
                    LogOutput::StdErr { message } => {
                        if errtxt.len() < 4000 {
                            errtxt.push_str(&String::from_utf8_lossy(&message));
                        }
                    }
                    _ => {}
                }
            }
        }
        file.flush()?;
        let code = self.exec_code(&exec).await;
        Ok((written, code, errtxt))
    }

    /// Código de salida de un exec; espera a que termine de verdad (el stream cierra un poco antes).
    pub async fn exec_code(&self, exec: &str) -> Option<i64> {
        for _ in 0..50 {
            match self.d.inspect_exec(exec).await {
                Ok(i) if i.running == Some(true) => tokio::time::sleep(Duration::from_millis(100)).await,
                Ok(i) => return i.exit_code,
                Err(_) => return None,
            }
        }
        None
    }

    /// Crea un exec con TTY (stdin opcional). Devuelve el id; luego `start_exec`. Con TTY, cerrar la
    /// conexión manda SIGHUP al proceso: así se puede cancelar un comando.
    pub async fn create_tty_exec(&self, id: &str, cmd: Vec<String>, stdin: bool, size: Option<(u16, u16)>) -> Result<String> {
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdin: Some(stdin),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    console_size: size.map(|(w, h)| vec![h as usize, w as usize]),
                    env: Some(vec!["TERM=xterm-256color".into(), "PAGER=cat".into(), "GIT_PAGER=cat".into()]),
                    cmd: Some(cmd),
                    ..Default::default()
                },
            )
            .await
            .map_err(explain_exec_error)?;
        Ok(exec.id)
    }

    /// Alimenta `sh -c script` con el contenido de un archivo (restauración SQL). Devuelve la cola de la salida.
    pub async fn exec_from_file(&self, id: &str, script: &str, path: &Path, progress: Prog<'_>) -> Result<String> {
        use tokio::io::AsyncWriteExt;
        let size = std::fs::metadata(path).with_context(|| format!("no existe {}", path.display()))?.len();
        // head -c: el comando recibe fin de archivo aunque el daemon no cierre stdin a tiempo
        let full = format!("head -c {size} | ({script})");
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(vec!["sh".into(), "-c".into(), full]),
                    ..Default::default()
                },
            )
            .await?
            .id;
        let StartExecResults::Attached { mut output, mut input } = self.d.start_exec(&exec, None).await? else {
            bail!("el daemon no adjuntó la entrada");
        };
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = sent.clone();
        let path = path.to_path_buf();
        let writer = tokio::spawn(async move {
            let mut f = tokio::fs::File::open(&path).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut f, &mut buf).await?;
                if n == 0 {
                    break;
                }
                input.write_all(&buf[..n]).await?;
                counter.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            }
            input.flush().await?;
            let _ = input.shutdown().await;
            Ok::<(), std::io::Error>(())
        });
        let mut tail = String::new();
        loop {
            tokio::select! {
                chunk = output.next() => match chunk {
                    Some(Ok(c)) => {
                        tail.push_str(&c.to_string());
                        if tail.len() > 8000 {
                            let mut cut = tail.len() - 6000;
                            while !tail.is_char_boundary(cut) {
                                cut += 1;
                            }
                            tail.drain(..cut);
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                _ = tokio::time::sleep(Duration::from_millis(300)) => {
                    let s = sent.load(std::sync::atomic::Ordering::Relaxed);
                    progress(format!("↑ restaurando  {}% ({}/{})", s * 100 / size.max(1), human_bytes(s), human_bytes(size)));
                }
            }
        }
        let wr = writer.await.map_err(|e| anyhow!("{e}"))?;
        let code = self.exec_code(&exec).await;
        if let Some(c) = code.filter(|c| *c != 0) {
            let clean: Vec<String> = tail.trim().lines().map(strip_ansi).collect();
            bail!("la restauración falló (código {c}):\n{}", clean.join("\n"));
        }
        wr.context("no se pudo enviar el archivo")?;
        Ok(tail)
    }

}
