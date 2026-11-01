//! Tareas en segundo plano: registros en vivo, comandos del contenedor y del host, Compose, Git y tareas de texto.

use super::*;

impl App {
    // ───────────────────────────── Tareas en segundo plano ─────────────────────────────

    /// Ejecuta una acción y publica su resultado como aviso; luego refresca.
    pub(super) fn bg<F>(&self, fut: F)
    where
        F: Future<Output = (String, Level)> + Send + 'static,
    {
        let tx = self.tx.clone();
        let n = self.shared.notify.clone();
        tokio::spawn(async move {
            let (m, l) = fut.await;
            let _ = tx.send(Msg::Toast(m, l));
            n.notify_one();
        });
    }

    pub(super) fn do_act(&mut self, id: String, name: String, act: Act, verb: &'static str) {
        self.progress = Some(format!("{verb} {name}…"));
        let e = self.engine.clone();
        self.bg(async move {
            match e.act(&id, act).await {
                Ok(()) => (format!("{name}: listo"), Level::Ok),
                Err(err) => (format!("{name}: {err}"), Level::Err),
            }
        });
    }

    pub fn start_logs(&mut self, id: &str, name: &str, timestamps: bool, keep: Option<(String, bool)>) -> LogView {
        self.log_session += 1;
        let session = self.log_session;
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        let idc = id.to_string();
        let handle = tokio::spawn(async move {
            let mut stream = Box::pin(engine.log_stream(&idc, "300", true, timestamps));
            let mut carry: Vec<u8> = Vec::new();
            let mut batch: Vec<String> = Vec::new();
            let mut tick = tokio::time::interval(Duration::from_millis(80));
            loop {
                tokio::select! {
                    item = stream.next() => match item {
                        Some(Ok(chunk)) => {
                            let is_err = matches!(chunk, bollard::container::LogOutput::StdErr { .. });
                            carry.extend_from_slice(chunk.as_ref());
                            while let Some(pos) = carry.iter().position(|b| *b == b'\n') {
                                let raw: Vec<u8> = carry.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&raw[..raw.len() - 1]).to_string();
                                let mut line = strip_ansi(&line);
                                if line.chars().count() > 4000 {
                                    line = line.chars().take(4000).collect::<String>() + "…";
                                }
                                batch.push(if is_err { format!("\u{1}{line}") } else { line });
                            }
                            if batch.len() >= 400 {
                                let _ = tx.send(Msg::Log { session, lines: std::mem::take(&mut batch) });
                            }
                        }
                        Some(Err(e)) => {
                            if !batch.is_empty() { let _ = tx.send(Msg::Log { session, lines: std::mem::take(&mut batch) }); }
                            let _ = tx.send(Msg::LogEnd { session, msg: e.to_string(), ok: false });
                            return;
                        }
                        None => {
                            if !carry.is_empty() {
                                batch.push(strip_ansi(&String::from_utf8_lossy(&carry)));
                            }
                            if !batch.is_empty() { let _ = tx.send(Msg::Log { session, lines: std::mem::take(&mut batch) }); }
                            let _ = tx.send(Msg::LogEnd { session, msg: "el contenedor dejó de emitir logs".into(), ok: false });
                            return;
                        }
                    },
                    _ = tick.tick() => {
                        if !batch.is_empty() {
                            let _ = tx.send(Msg::Log { session, lines: std::mem::take(&mut batch) });
                        }
                    }
                }
            }
        });
        let (filter, wrap) = keep.unwrap_or((String::new(), false));
        LogView {
            kind: LogKind::Container,
            id: id.to_string(),
            name: name.to_string(),
            session,
            lines: VecDeque::new(),
            follow: true,
            scroll: 0,
            hscroll: 0,
            filter,
            filter_input: None,
            wrap,
            timestamps,
            ended: None,
            ended_ok: false,
            total: 0,
            handle: Some(handle),
        }
    }

    /// Vista de salida en vivo para una tarea: `f` recibe una función que publica líneas y devuelve
    /// el mensaje final. Cerrar la vista (Esc) cancela la tarea.
    pub fn start_task_view<F, Fut>(&mut self, title: String, f: F) -> LogView
    where
        F: FnOnce(Box<dyn FnMut(String) + Send>) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.log_session += 1;
        let session = self.log_session;
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            let txl = tx.clone();
            let out: Box<dyn FnMut(String) + Send> = Box::new(move |l: String| {
                let l = if l.chars().count() > 4000 { l.chars().take(4000).collect::<String>() + "…" } else { l };
                let _ = txl.send(Msg::Log { session, lines: vec![l] });
            });
            let (msg, ok) = match f(out).await {
                Ok(m) => (m, true),
                Err(e) => (format!("{e:#}"), false),
            };
            let _ = tx.send(Msg::LogEnd { session, msg, ok });
        });
        LogView {
            kind: LogKind::Command,
            id: String::new(),
            name: title,
            session,
            lines: VecDeque::new(),
            follow: true,
            scroll: 0,
            hscroll: 0,
            filter: String::new(),
            filter_input: None,
            wrap: true,
            timestamps: false,
            ended: None,
            ended_ok: false,
            total: 0,
            handle: Some(handle),
        }
    }

    /// Comando dentro de un contenedor con salida en vivo (con TTY: Esc lo interrumpe).
    pub(super) fn run_exec(&mut self, id: String, name: String, line: String) {
        self.remember_cmd(&name, &line);
        let e = self.engine.clone();
        let lv = self.start_task_view(format!("{name} $ {line}"), move |mut out| async move {
            let exec = e.create_tty_exec(&id, vec!["sh".into(), "-c".into(), line], false, Some((200, 50))).await?;
            let opts = bollard::exec::StartExecOptions { detach: false, tty: true, output_capacity: None };
            let mut seen = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = e.d.start_exec(&exec, Some(opts)).await? {
                let mut carry: Vec<u8> = Vec::new();
                while let Some(chunk) = output.next().await {
                    carry.extend_from_slice(chunk?.as_ref());
                    while let Some(pos) = carry.iter().position(|b| *b == b'\n') {
                        let raw: Vec<u8> = carry.drain(..=pos).collect();
                        let l = strip_ansi(&String::from_utf8_lossy(&raw[..raw.len() - 1]));
                        if seen.len() < 2000 {
                            seen.push_str(&l);
                        }
                        out(l);
                    }
                }
                if !carry.is_empty() {
                    out(strip_ansi(&String::from_utf8_lossy(&carry)));
                }
            }
            match e.exec_code(&exec).await {
                Some(0) => Ok("✔ terminó (código 0)".into()),
                Some(c) => match crate::engine::missing_binary_hint(&seen) {
                    Some(h) if c == 126 || c == 127 => anyhow::bail!("{h}"),
                    _ => anyhow::bail!("terminó con código {c}"),
                },
                None => Ok("terminó".into()),
            }
        });
        self.mode = Mode::Logs(Box::new(lv));
    }

    /// Comando en la máquina (local, o el servidor si el daemon es remoto por SSH).
    pub(super) fn run_host(&mut self, line: String) {
        self.remember_cmd("", &line);
        let e = self.engine.clone();
        let where_ = e.ssh_target().map(|(t, _)| t).unwrap_or_else(|| "host".into());
        let lv = self.start_task_view(format!("{where_} $ {line}"), move |mut out| async move {
            crate::ext::run_lines(crate::ext::host_command(&e, &line), "el comando", &mut out).await?;
            Ok("✔ terminó (código 0)".into())
        });
        self.mode = Mode::Logs(Box::new(lv));
    }

    pub(super) fn remember_cmd(&mut self, key: &str, line: &str) {
        let h = self.cmd_hist.entry(key.to_string()).or_default();
        h.retain(|x| x != line);
        h.push(line.to_string());
        if h.len() > 50 {
            h.remove(0);
        }
    }

    pub(super) fn start_compose(&mut self, path: String) {
        let e = self.engine.clone();
        let lv = self.start_task_view(format!("compose up · {path}"), move |mut out| async move {
            let p = std::path::PathBuf::from(shell_path(&path));
            let file = crate::ext::find_compose_file(&p).ok_or_else(|| anyhow::anyhow!("no encontré compose.yaml ni docker-compose.yml en {}", p.display()))?;
            let file = std::fs::canonicalize(&file)?;
            let project = crate::ext::project_name(&file.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().to_string()).unwrap_or_default());
            crate::ext::compose(&e, &project, &[file], &["up", "-d", "--remove-orphans"], &mut out).await?;
            Ok(format!("✔ proyecto «{project}» levantado (aparece como stack en Contenedores)"))
        });
        self.mode = Mode::Logs(Box::new(lv));
    }

    /// «url[#rama]»: Compose si el repositorio lo trae; si no, construye su Dockerfile y abre el formulario.
    pub(super) fn start_git(&mut self, spec: String) {
        let (url, git_ref) = match spec.rsplit_once('#') {
            Some((u, r)) if !r.is_empty() => (u.to_string(), Some(r.to_string())),
            _ => (spec.clone(), None),
        };
        // si pegaron «https://usuario:token@…», el token no debe verse en el título ni guardarse
        let src = crate::ext::GitSource::new(&url, git_ref, String::new(), None, None, None);
        let (url, git_ref, auth) = (src.url, src.git_ref, src.auth);
        let name = suggest_name(&self.snap.containers, &crate::cli::repo_name(&url));
        let tmpl = self.catalog.get("custom").cloned().unwrap_or_default();
        let (tz, restart) = (self.cfg.tz.clone(), self.cfg.restart());
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let lv = self.start_task_view(format!("git · {url}"), move |mut out| async move {
            let src = crate::ext::GitSource { url, git_ref, name, subdir: None, auth, env: vec![] };
            match crate::ext::git_deploy(&e, &src, &tmpl, tz.as_deref(), &restart, &mut out).await? {
                crate::ext::GitOutcome::Compose => Ok(format!("✔ «{}» levantado con Compose", src.name)),
                crate::ext::GitOutcome::Image(plan) => {
                    let _ = tx.send(Msg::DeployForm(plan));
                    Ok("✔ imagen construida".into())
                }
            }
        });
        self.mode = Mode::Logs(Box::new(lv));
    }

    pub(super) fn open_text_task<F>(&mut self, busy: String, title: String, fut: F)
    where
        F: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.progress = Some(busy);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match fut.await {
                Ok(body) => {
                    let _ = tx.send(Msg::Text { title, body });
                }
                Err(err) => {
                    let _ = tx.send(Msg::Toast(format!("{err:#}"), Level::Err));
                }
            }
        });
    }

}
