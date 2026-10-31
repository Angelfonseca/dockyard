//! Operaciones que abren formularios o lanzan acciones sobre contenedores, imágenes y volúmenes.

use super::*;

impl App {
    pub(super) fn open_creds(&mut self, target: String) {
        let e = self.engine.clone();
        let t = target.clone();
        self.open_text_task(format!("Leyendo credenciales de {target}…"), format!("Credenciales · {target}"), async move {
            let creds = e.credentials(&t).await?;
            if creds.is_empty() {
                return Ok(format!("«{t}» no tiene variables de contraseña o token."));
            }
            let mut body = String::from("Contraseñas y tokens (de la configuración de cada contenedor):\n\n");
            for (c, k, v) in creds {
                body.push_str(&format!("  {c:<22} {k} = {v}\n"));
            }
            Ok(body)
        });
    }

    pub(super) fn open_volume_du(&mut self, name: String) {
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let n = name.clone();
        self.open_text_task(format!("Midiendo {name}…"), format!("Volumen · {name}"), async move {
            let mut prog = move |s: String| {
                let _ = tx.send(Msg::Progress(s));
            };
            e.volume_du(&n, &mut prog).await
        });
    }

    pub(super) fn start_clone_volume(&mut self, src: String, dst: String) {
        self.progress = Some(format!("Copiando {src} → {dst}…"));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let u = self.shared.usage_now.clone();
        self.bg(async move {
            let mut prog = move |s: String| {
                let _ = tx.send(Msg::Progress(s));
            };
            let r = match e.clone_volume(&src, &dst, &mut prog).await {
                Ok(()) => (format!("«{src}» copiado en «{dst}»"), Level::Ok),
                Err(err) => (format!("{err:#}"), Level::Err),
            };
            u.notify_one();
            r
        });
    }

    pub(super) fn start_restore(&mut self, a: ConfirmAction, clean: bool) {
        let e = self.engine.clone();
        let tx = self.tx.clone();
        self.progress = Some("Restaurando…".into());
        self.bg(async move {
            let mut prog = move |s: String| {
                let _ = tx.send(Msg::Progress(s));
            };
            match a {
                ConfirmAction::RestoreSql { id, name, script, file } => match e.exec_from_file(&id, &script, &file, &mut prog).await {
                    Ok(_) => (format!("{} restaurado en «{name}»", file.display()), Level::Ok),
                    Err(err) => (format!("{name}: {err:#}"), Level::Err),
                },
                ConfirmAction::RestoreVolume { name, file } => match e.restore_volume(&name, &file, clean, &mut prog).await {
                    Ok(()) => (format!("{} restaurado en «{name}»", file.display()), Level::Ok),
                    Err(err) => (format!("{name}: {err:#}"), Level::Err),
                },
                _ => (String::new(), Level::Info),
            }
        });
    }

    pub(super) fn open_inspect(&mut self, id: String, name: String) {
        self.progress = Some(format!("Leyendo {name}…"));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match e.inspect_text(&id).await {
                Ok(t) => {
                    let _ = tx.send(Msg::Text { title: format!("inspect · {name}"), body: t });
                }
                Err(err) => {
                    let _ = tx.send(Msg::Toast(format!("{name}: {err}"), Level::Err));
                }
            }
        });
    }

    pub(super) fn open_edit(&mut self, id: String, name: String) {
        self.progress = Some(format!("Cargando {name}…"));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match e.load_edit(&id).await {
                Ok((src, plan)) => {
                    let _ = tx.send(Msg::EditLoaded(Box::new(src), Box::new(plan)));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Toast(format!("{name}: {err}"), Level::Err));
                }
            }
        });
    }

    pub(super) fn start_backup_container(&mut self, c: &CRow) {
        let Some((script, ext)) = self.catalog.dump_for_image(&c.image) else {
            self.toast(
                "Sin comando de respaldo conocido para esta imagen. Usa el respaldo de volumen (pestaña Volúmenes, tecla b).",
                Level::Warn,
            );
            return;
        };
        if !c.is_up() {
            self.toast("El contenedor debe estar activo para respaldarlo", Level::Warn);
            return;
        }
        self.progress = Some(format!("Respaldando {}…", c.name));
        let e = self.engine.clone();
        let dir = self.cfg.backups_dir();
        let (id, name) = (c.id.clone(), c.name.clone());
        self.bg(async move {
            match e.backup_dump(&id, &name, &script, &ext, &dir).await {
                Ok((p, n)) => (format!("Respaldo guardado: {} ({})", p.display(), human_bytes(n)), Level::Ok),
                Err(err) => (format!("{name}: {err}"), Level::Err),
            }
        });
    }

    pub(super) fn start_backup_volume(&mut self, name: String) {
        self.progress = Some(format!("Respaldando volumen {name}…"));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let dir = self.cfg.backups_dir();
        self.bg(async move {
            let txp = tx.clone();
            let mut prog = move |s: String| {
                let _ = txp.send(Msg::Progress(s));
            };
            match e.backup_volume(&name, &dir, &mut prog).await {
                Ok((p, n)) => (format!("Respaldo guardado: {} ({})", p.display(), human_bytes(n)), Level::Ok),
                Err(err) => (format!("{name}: {err:#}"), Level::Err),
            }
        });
    }

    pub(super) fn start_pull(&mut self, image: String) {
        self.progress = Some(format!("↓ {image}  iniciando…"));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        self.bg(async move {
            let txp = tx.clone();
            let mut prog = move |s: String| {
                let _ = txp.send(Msg::Progress(s));
            };
            match e.pull(&image, &mut prog).await {
                Ok(()) => (format!("Imagen descargada: {image}"), Level::Ok),
                Err(err) => (format!("{image}: {err:#}"), Level::Err),
            }
        });
    }

    pub(super) fn start_update(&mut self, id: String, name: String) {
        self.progress = Some(format!("Actualizando {name}…"));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        self.bg(async move {
            let txp = tx.clone();
            let mut prog = move |s: String| {
                let _ = txp.send(Msg::Progress(s));
            };
            match crate::ext::update_container(&e, &id, &mut prog).await {
                Ok(UpdateOutcome::UpToDate) => (format!("{name}: ya está al día"), Level::Info),
                Ok(UpdateOutcome::Updated) => (format!("{name}: actualizado"), Level::Ok),
                Err(err) => (format!("{name}: {err:#}"), Level::Err),
            }
        });
    }

    pub(super) fn start_deploy(&mut self, plan: Plan) {
        self.progress = Some(format!("Desplegando {}…", plan.name));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let n = self.shared.notify.clone();
        tokio::spawn(async move {
            let txp = tx.clone();
            let mut prog = move |s: String| {
                let _ = txp.send(Msg::Progress(s));
            };
            match crate::ops::deploy(&e, &plan, &mut prog).await {
                Ok(rep) => {
                    let mut body = format!("✔ «{}» desplegado ({} contenedor{})\n\n", plan.name, rep.containers.len(), if rep.containers.len() == 1 { "" } else { "es" });
                    body.push_str("Contenedores:\n");
                    for c in &rep.containers {
                        body.push_str(&format!("  • {c}\n"));
                    }
                    if let Some(net) = &rep.network {
                        body.push_str(&format!("\nRed interna: {net}\n"));
                    }
                    if !rep.ports.is_empty() {
                        body.push_str("\nPuertos publicados:\n");
                        for p in &rep.ports {
                            body.push_str(&format!("  • {p}\n"));
                        }
                    }
                    if !rep.credentials.is_empty() {
                        body.push_str("\nCredenciales generadas (también puedes verlas después con la tecla C):\n");
                        for (c, k, v) in &rep.credentials {
                            body.push_str(&format!("  {c:<22} {k} = {v}\n"));
                        }
                    }
                    for w in &rep.warnings {
                        body.push_str(&format!("\n! {w}\n"));
                    }
                    if !rep.note.is_empty() {
                        body.push_str(&format!("\nNota: {}\n", rep.note));
                    }
                    let _ = tx.send(Msg::Text { title: format!("Despliegue · {}", plan.name), body });
                }
                Err(err) => {
                    let _ = tx.send(Msg::Text { title: "Error al desplegar".into(), body: format!("{err:#}") });
                }
            }
            n.notify_one();
        });
    }

    pub(super) fn start_recreate(&mut self, src: crate::engine::EditSource, plan: Plan) {
        self.progress = Some(format!("Recreando {}…", src.name));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let n = self.shared.notify.clone();
        tokio::spawn(async move {
            let txp = tx.clone();
            let mut prog = move |s: String| {
                let _ = txp.send(Msg::Progress(s));
            };
            match e.recreate(&src, &plan, &mut prog).await {
                Ok(_) => {
                    let _ = tx.send(Msg::Toast(format!("{}: recreado con los cambios", src.name), Level::Ok));
                }
                Err(err) => {
                    let _ = tx.send(Msg::Text { title: "Error al recrear".into(), body: format!("{err:#}") });
                }
            }
            n.notify_one();
        });
    }

}
