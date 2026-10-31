//! Teclas de cada pestaña: contenedores, imágenes, volúmenes y catálogo.

use super::*;

impl App {
    pub(super) fn key_containers(&mut self, key: KeyEvent) {
        if self.nav(key, Tab::Containers) {
            return;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Some(c) = self.cur_container().cloned() else {
            match key.code {
                KeyCode::Char('/') => self.open_filter(Tab::Containers),
                KeyCode::Esc => {
                    self.clear_filter(Tab::Containers);
                }
                KeyCode::Char('a') => self.show_all = !self.show_all,
                KeyCode::Char('n') => self.goto(Tab::Catalog),
                _ => {}
            }
            return;
        };
        match key.code {
            KeyCode::Enter | KeyCode::Char('l') => {
                let lv = self.start_logs(&c.id, &c.name, false, None);
                self.mode = Mode::Logs(Box::new(lv));
            }
            KeyCode::Char('s') => {
                if c.is_up() {
                    self.do_act(c.id.clone(), c.name.clone(), Act::Stop, "Deteniendo");
                } else {
                    self.do_act(c.id.clone(), c.name.clone(), Act::Start, "Iniciando");
                }
            }
            KeyCode::Char('r') => self.do_act(c.id.clone(), c.name.clone(), Act::Restart, "Reiniciando"),
            KeyCode::Char('p') => {
                if c.state == St::Paused {
                    self.do_act(c.id.clone(), c.name.clone(), Act::Unpause, "Reanudando");
                } else if c.state == St::Running {
                    self.do_act(c.id.clone(), c.name.clone(), Act::Pause, "Pausando");
                }
            }
            KeyCode::Char('K') => {
                self.mode = Mode::Confirm(Box::new(Confirm {
                    title: "Matar contenedor".into(),
                    lines: vec![format!("Enviar SIGKILL a «{}» (sin apagado limpio).", c.name)],
                    action: ConfirmAction::KillContainer { id: c.id.clone(), name: c.name.clone() },
                    danger: true,
                    alt: None,
                }));
            }
            KeyCode::Char('D') if shift && c.stack.is_some() => {
                let st = c.stack.clone().unwrap_or_default();
                let n = self.snap.containers.iter().filter(|x| x.stack.as_deref() == Some(st.as_str())).count();
                self.mode = Mode::Confirm(Box::new(Confirm {
                    title: format!("Eliminar stack «{st}»"),
                    lines: vec![
                        format!("Se eliminarán los {n} contenedores del stack y su red interna."),
                        "Los volúmenes se conservan (bórralos desde la pestaña Volúmenes).".into(),
                    ],
                    action: ConfirmAction::DownStack { name: st },
                    danger: true,
                    alt: None,
                }));
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let mut lines = vec![format!("Se eliminará «{}». Los volúmenes se conservan.", c.name)];
                if c.is_up() {
                    lines.push("Está en ejecución: se detendrá primero.".into());
                }
                self.mode = Mode::Confirm(Box::new(Confirm {
                    title: "Eliminar contenedor".into(),
                    lines,
                    action: ConfirmAction::RemoveContainer { id: c.id.clone(), name: c.name.clone() },
                    danger: true,
                    alt: None,
                }));
            }
            KeyCode::Char('e') => self.open_edit(c.id.clone(), c.name.clone()),
            KeyCode::Char('u') => {
                self.mode = Mode::Confirm(Box::new(Confirm {
                    title: "Actualizar imagen".into(),
                    lines: vec![
                        format!("Descargar la última versión de {} y recrear «{}» si cambió.", c.image, c.name),
                        "Las variables, puertos y volúmenes se conservan.".into(),
                    ],
                    action: ConfirmAction::UpdateContainer { id: c.id.clone(), name: c.name.clone() },
                    danger: false,
                    alt: None,
                }));
            }
            KeyCode::Char('x') => {
                if c.is_up() {
                    self.pending = Some(Pending::Shell { id: c.id.clone(), name: c.name.clone() });
                } else {
                    self.toast("El contenedor no está activo", Level::Warn);
                }
            }
            KeyCode::Char('c') => {
                if c.is_up() {
                    self.hist_pos = None;
                    self.mode = Mode::Prompt(Box::new(Prompt {
                        title: format!("Ejecutar en {}", c.name),
                        hint: "se ejecuta con sh -c · salida en vivo · ↑↓ historial · Esc en la salida lo detiene".into(),
                        input: LineInput::new(""),
                        action: PromptAction::Exec { id: c.id.clone(), name: c.name.clone() },
                    }));
                } else {
                    self.toast("El contenedor no está activo", Level::Warn);
                }
            }
            KeyCode::Char('F') => {
                self.mode = Mode::Prompt(Box::new(Prompt {
                    title: format!("Editar archivo en {}", c.name),
                    hint: "ruta absoluta dentro del contenedor (funciona aunque esté detenido) · si no existe se crea".into(),
                    input: LineInput::new("/"),
                    action: PromptAction::EditFile { id: c.id.clone(), name: c.name.clone(), volume: false },
                }));
            }
            KeyCode::Char('R') => match self.catalog.restore_for_image(&c.image) {
                Some((script, ext)) => {
                    let dir = self.cfg.backups_dir();
                    let def = latest_backup(&dir, &c.name, &ext).unwrap_or_else(|| dir.join(""));
                    self.mode = Mode::Prompt(Box::new(Prompt {
                        title: format!("Restaurar base de datos en {}", c.name),
                        hint: format!("archivo .{ext} a cargar (por defecto, el respaldo más reciente)"),
                        input: LineInput::new(&def.display().to_string()),
                        action: PromptAction::Restore { target: RestoreTarget::Sql { id: c.id.clone(), name: c.name.clone(), script } },
                    }));
                }
                None => self.toast("Sin comando de restauración conocido para esta imagen: restaura su volumen (pestaña Volúmenes, R)", Level::Warn),
            },
            KeyCode::Char('C') => self.open_creds(c.stack.clone().unwrap_or_else(|| c.name.clone())),
            KeyCode::Char('i') => self.open_inspect(c.id.clone(), c.name.clone()),
            KeyCode::Char('b') => self.start_backup_container(&c),
            KeyCode::Char('a') => self.show_all = !self.show_all,
            KeyCode::Char('o') => self.sort = self.sort.next(),
            KeyCode::Char('n') => self.goto(Tab::Catalog),
            KeyCode::Char('/') => self.open_filter(Tab::Containers),
            KeyCode::Esc => {
                self.clear_filter(Tab::Containers);
            }
            _ => {}
        }
    }

    pub(super) fn key_images(&mut self, key: KeyEvent) {
        if self.nav(key, Tab::Images) {
            return;
        }
        match key.code {
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.mode = Mode::Prompt(Box::new(Prompt {
                    title: "Descargar imagen".into(),
                    hint: "ej. postgres:17-alpine · ghcr.io/usuario/app:latest".into(),
                    input: LineInput::new(""),
                    action: PromptAction::Pull,
                }));
            }
            KeyCode::Enter | KeyCode::Char('n') => {
                if let Some(i) = self.cur_image().cloned() {
                    if let Some(tag) = i.tags.first() {
                        self.mode = Mode::Prompt(Box::new(Prompt {
                            title: format!("Nuevo contenedor desde {tag}"),
                            hint: "nombre del contenedor".into(),
                            input: LineInput::new(&suggest_name(&self.snap.containers, &image_repo(tag).replace('/', "-"))),
                            action: PromptAction::DeployName { template: "custom".into(), image: Some(tag.clone()) },
                        }));
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(i) = self.cur_image().cloned() {
                    let label = i.tags.first().cloned().unwrap_or_else(|| short_id(&i.id).to_string());
                    if !i.used_by.is_empty() {
                        self.toast(format!("{label} está en uso por: {}", i.used_by.join(", ")), Level::Warn);
                    } else {
                        self.mode = Mode::Confirm(Box::new(Confirm {
                            title: "Eliminar imagen".into(),
                            lines: vec![format!("Se eliminará {label} ({}).", human_bytes(i.size))],
                            action: ConfirmAction::RemoveImage { id: i.id.clone(), label },
                            danger: true,
                            alt: None,
                        }));
                    }
                }
            }
            KeyCode::Char('D') => {
                let free: u64 = self.images.iter().filter(|i| i.used_by.is_empty()).map(|i| i.size).sum();
                let n = self.images.iter().filter(|i| i.used_by.is_empty()).count();
                self.mode = Mode::Confirm(Box::new(Confirm {
                    title: "Limpiar imágenes sin usar".into(),
                    lines: vec![
                        format!("Se eliminarán {n} imágenes que ningún contenedor usa (≈ {}).", human_bytes(free)),
                        "Se volverán a descargar si las necesitas.".into(),
                    ],
                    action: ConfirmAction::PruneImages,
                    danger: true,
                    alt: None,
                }));
            }
            KeyCode::Char('/') => self.open_filter(Tab::Images),
            KeyCode::Esc => {
                self.clear_filter(Tab::Images);
            }
            _ => {}
        }
    }

    pub(super) fn key_volumes(&mut self, key: KeyEvent) {
        if self.nav(key, Tab::Volumes) {
            return;
        }
        match key.code {
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(v) = self.cur_volume().cloned() {
                    if !v.used_by.is_empty() {
                        self.toast(format!("{} está en uso por: {}", v.name, v.used_by.join(", ")), Level::Warn);
                    } else {
                        self.mode = Mode::Confirm(Box::new(Confirm {
                            title: "Eliminar volumen".into(),
                            lines: vec![
                                format!("Se borrará «{}» y todos sus datos.", v.name),
                                "Esta acción no se puede deshacer. ¿Tienes respaldo? (tecla b)".into(),
                            ],
                            action: ConfirmAction::RemoveVolume { name: v.name.clone() },
                            danger: true,
                            alt: None,
                        }));
                    }
                }
            }
            KeyCode::Char('D') => {
                let names: Vec<String> = self.volumes.iter().filter(|v| v.used_by.is_empty()).map(|v| v.name.clone()).collect();
                if names.is_empty() {
                    self.toast("No hay volúmenes huérfanos", Level::Info);
                } else {
                    let mut lines = vec![format!("Se borrarán {} volúmenes que ningún contenedor usa:", names.len())];
                    for n in names.iter().take(8) {
                        lines.push(format!("  • {}", truncate(n, 60)));
                    }
                    if names.len() > 8 {
                        lines.push(format!("  … y {} más", names.len() - 8));
                    }
                    lines.push("Sus datos se perderán.".into());
                    self.mode = Mode::Confirm(Box::new(Confirm {
                        title: "Limpiar volúmenes huérfanos".into(),
                        lines,
                        action: ConfirmAction::PruneVolumes { names },
                        danger: true,
                        alt: None,
                    }));
                }
            }
            KeyCode::Char('b') => {
                if let Some(v) = self.cur_volume().cloned() {
                    self.start_backup_volume(v.name);
                }
            }
            KeyCode::Enter => {
                if let Some(v) = self.cur_volume().cloned() {
                    self.open_volume_du(v.name);
                }
            }
            KeyCode::Char('f') => {
                if let Some(v) = self.cur_volume().cloned() {
                    self.mode = Mode::Prompt(Box::new(Prompt {
                        title: format!("Editar archivo en el volumen {}", v.name),
                        hint: "ruta dentro del volumen (ej. config/app.conf) · si no existe se crea".into(),
                        input: LineInput::new(""),
                        action: PromptAction::EditFile { id: v.name.clone(), name: v.name.clone(), volume: true },
                    }));
                }
            }
            KeyCode::Char('c') => {
                if let Some(v) = self.cur_volume().cloned() {
                    let existing: HashSet<&str> = self.volumes.iter().map(|x| x.name.as_str()).collect();
                    let mut def = format!("{}-copia", v.name);
                    let mut i = 2;
                    while existing.contains(def.as_str()) {
                        def = format!("{}-copia{i}", v.name);
                        i += 1;
                    }
                    self.mode = Mode::Prompt(Box::new(Prompt {
                        title: format!("Clonar volumen {}", v.name),
                        hint: "nombre del volumen nuevo (copia exacta, con permisos)".into(),
                        input: LineInput::new(&def),
                        action: PromptAction::CloneVolume { src: v.name.clone() },
                    }));
                }
            }
            KeyCode::Char('R') => {
                if let Some(v) = self.cur_volume().cloned() {
                    let dir = self.cfg.backups_dir();
                    let def = latest_backup(&dir, &v.name, "tar.gz").unwrap_or_else(|| dir.join(""));
                    self.mode = Mode::Prompt(Box::new(Prompt {
                        title: format!("Restaurar volumen {}", v.name),
                        hint: "archivo .tar.gz (por defecto, el respaldo más reciente)".into(),
                        input: LineInput::new(&def.display().to_string()),
                        action: PromptAction::Restore { target: RestoreTarget::Volume { name: v.name.clone(), used_by: v.used_by.clone() } },
                    }));
                }
            }
            KeyCode::Char('/') => self.open_filter(Tab::Volumes),
            KeyCode::Esc => {
                self.clear_filter(Tab::Volumes);
            }
            _ => {}
        }
    }

    pub(super) fn key_catalog(&mut self, key: KeyEvent) {
        if self.nav(key, Tab::Catalog) {
            return;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('n') | KeyCode::Char('i') => {
                if let Some(t) = self.cur_template().cloned() {
                    self.mode = Mode::Prompt(Box::new(Prompt {
                        title: format!("Instalar «{}»", t.name),
                        hint: "nombre del stack (letras, números, - _ .)".into(),
                        input: LineInput::new(&suggest_name(&self.snap.containers, &t.id)),
                        action: PromptAction::DeployName { template: t.id.clone(), image: None },
                    }));
                }
            }
            KeyCode::Char('r') => {
                self.mode = Mode::Prompt(Box::new(Prompt {
                    title: "Traer desde Git".into(),
                    hint: "URL[#rama] · privado: https://usuario:token@host/ruta · con compose.yaml se levanta; con Dockerfile se construye".into(),
                    input: LineInput::new(""),
                    action: PromptAction::Git,
                }));
            }
            KeyCode::Char('o') => {
                self.mode = Mode::Prompt(Box::new(Prompt {
                    title: "Levantar proyecto Compose".into(),
                    hint: "carpeta con compose.yaml o ruta al archivo (necesita docker compose)".into(),
                    input: LineInput::new(&std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default()),
                    action: PromptAction::Compose,
                }));
            }
            KeyCode::Char('/') => self.open_filter(Tab::Catalog),
            KeyCode::Esc => {
                self.clear_filter(Tab::Catalog);
            }
            _ => {}
        }
    }

}
