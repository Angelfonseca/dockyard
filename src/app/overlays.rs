//! Teclas de las superposiciones: texto, logs, avisos, confirmaciones y formularios.

use super::*;

impl App {
    pub(super) fn key_text(&mut self, mut tv: Box<TextView>, key: KeyEvent) -> Mode {
        let page = 15usize;
        let max = tv.lines.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') => tv.scroll = tv.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => tv.scroll = (tv.scroll + 1).min(max),
            KeyCode::PageUp => tv.scroll = tv.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => tv.scroll = (tv.scroll + page).min(max),
            KeyCode::Home | KeyCode::Char('g') => tv.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => tv.scroll = max,
            KeyCode::Left | KeyCode::Char('h') => tv.hscroll = tv.hscroll.saturating_sub(8),
            KeyCode::Right | KeyCode::Char('l') => tv.hscroll += 8,
            _ => {}
        }
        Mode::Text(tv)
    }

    pub(super) fn key_logs(&mut self, mut lv: Box<LogView>, key: KeyEvent) -> Mode {
        if let Some(inp) = &mut lv.filter_input {
            match key.code {
                KeyCode::Enter => {
                    lv.filter = inp.text();
                    lv.filter_input = None;
                    lv.follow = true;
                }
                KeyCode::Esc => lv.filter_input = None,
                _ => {
                    inp.handle(key);
                }
            }
            return Mode::Logs(lv);
        }
        let page = 20usize;
        let filtered = filtered_len(&lv);
        let max = filtered.saturating_sub(1);
        // posición real cuando se está siguiendo: el final
        let cur = if lv.follow { max } else { lv.scroll.min(max) };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Mode::Normal,
            KeyCode::Char('f') | KeyCode::Char(' ') => lv.follow = !lv.follow,
            KeyCode::Up | KeyCode::Char('k') => {
                lv.scroll = cur.saturating_sub(1);
                lv.follow = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                lv.scroll = (cur + 1).min(max);
                if lv.scroll >= max {
                    lv.follow = true;
                }
            }
            KeyCode::PageUp => {
                lv.scroll = cur.saturating_sub(page);
                lv.follow = false;
            }
            KeyCode::PageDown => {
                lv.scroll = (cur + page).min(max);
                if lv.scroll >= max {
                    lv.follow = true;
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                lv.scroll = 0;
                lv.follow = false;
            }
            KeyCode::End | KeyCode::Char('G') => lv.follow = true,
            KeyCode::Left | KeyCode::Char('h') => lv.hscroll = lv.hscroll.saturating_sub(8),
            KeyCode::Right | KeyCode::Char('l') => lv.hscroll += 8,
            KeyCode::Char('w') => {
                lv.wrap = !lv.wrap;
                lv.hscroll = 0;
            }
            KeyCode::Char('c') => {
                lv.lines.clear();
                lv.scroll = 0;
                lv.follow = true;
            }
            KeyCode::Char('/') => lv.filter_input = Some(LineInput::new(&lv.filter)),
            KeyCode::Char('t') if lv.kind == LogKind::Container => {
                let (id, name, ts) = (lv.id.clone(), lv.name.clone(), !lv.timestamps);
                let keep = (lv.filter.clone(), lv.wrap);
                drop(lv);
                let nlv = self.start_logs(&id, &name, ts, Some(keep));
                return Mode::Logs(Box::new(nlv));
            }
            _ => {}
        }
        Mode::Logs(lv)
    }

    pub(super) fn key_prompt(&mut self, mut p: Box<Prompt>, key: KeyEvent) -> Mode {
        let hist_key = match &p.action {
            PromptAction::Exec { name, .. } => Some(name.clone()),
            PromptAction::HostExec => Some(String::new()),
            _ => None,
        };
        match key.code {
            KeyCode::Esc => return Mode::Normal,
            KeyCode::Enter => return self.submit_prompt(*p),
            KeyCode::Up | KeyCode::Down if hist_key.is_some() => {
                let h = self.cmd_hist.get(&hist_key.unwrap_or_default()).cloned().unwrap_or_default();
                if !h.is_empty() {
                    let pos = match (self.hist_pos, key.code) {
                        (None, KeyCode::Up) => Some(h.len() - 1),
                        (Some(i), KeyCode::Up) => Some(i.saturating_sub(1)),
                        (Some(i), KeyCode::Down) if i + 1 < h.len() => Some(i + 1),
                        _ => None,
                    };
                    self.hist_pos = pos;
                    p.input = LineInput::new(pos.map(|i| h[i].as_str()).unwrap_or(""));
                }
            }
            _ => {
                p.input.handle(key);
            }
        }
        Mode::Prompt(p)
    }

    pub(super) fn submit_prompt(&mut self, p: Prompt) -> Mode {
        let text = p.input.text().trim().to_string();
        match p.action {
            PromptAction::Filter(tab) => {
                match tab {
                    Tab::Containers => self.c_filter = text,
                    Tab::Images => self.i_filter = text,
                    Tab::Volumes => self.v_filter = text,
                    _ => self.cat_filter = text,
                }
                Mode::Normal
            }
            PromptAction::Pull => {
                if text.is_empty() {
                    return Mode::Normal;
                }
                self.start_pull(text);
                Mode::Normal
            }
            PromptAction::Exec { id, name } => {
                if text.is_empty() {
                    return Mode::Normal;
                }
                self.run_exec(id, name, text);
                std::mem::replace(&mut self.mode, Mode::Normal)
            }
            PromptAction::HostExec => {
                if text.is_empty() {
                    return Mode::Normal;
                }
                self.run_host(text);
                std::mem::replace(&mut self.mode, Mode::Normal)
            }
            PromptAction::Git => {
                if text.is_empty() {
                    return Mode::Normal;
                }
                self.start_git(text);
                std::mem::replace(&mut self.mode, Mode::Normal)
            }
            PromptAction::Compose => {
                if text.is_empty() {
                    return Mode::Normal;
                }
                self.start_compose(text);
                std::mem::replace(&mut self.mode, Mode::Normal)
            }
            PromptAction::EditFile { id, name, volume } => {
                let path = text.trim().to_string();
                if path.is_empty() || path.ends_with('/') {
                    self.toast("Escribe la ruta de un archivo", Level::Warn);
                    return Mode::Prompt(Box::new(Prompt { title: p.title, hint: p.hint, input: p.input, action: PromptAction::EditFile { id, name, volume } }));
                }
                self.pending = Some(Pending::EditFile { id, name, path, volume });
                Mode::Normal
            }
            PromptAction::CloneVolume { src } => {
                if !valid_name(&text) {
                    self.toast("Nombre inválido: usa letras, números, - _ . (máx. 63)", Level::Err);
                    return Mode::Prompt(Box::new(Prompt { title: p.title, hint: p.hint, input: p.input, action: PromptAction::CloneVolume { src } }));
                }
                self.start_clone_volume(src, text);
                Mode::Normal
            }
            PromptAction::Restore { target } => {
                let file = std::path::PathBuf::from(shell_path(&text));
                if !file.is_file() {
                    self.toast(format!("No existe el archivo {}", file.display()), Level::Err);
                    return Mode::Prompt(Box::new(Prompt { title: p.title, hint: p.hint, input: p.input, action: PromptAction::Restore { target } }));
                }
                let size = std::fs::metadata(&file).map(|m| human_bytes(m.len())).unwrap_or_default();
                let confirm = match target {
                    RestoreTarget::Sql { id, name, script } => Confirm {
                        title: format!("Restaurar base de datos en «{name}»"),
                        lines: vec![
                            format!("Se cargará {} ({size}) en la base de datos.", file.display()),
                            "Puede sobrescribir datos existentes. ¿Tienes un respaldo reciente? (tecla b)".into(),
                        ],
                        action: ConfirmAction::RestoreSql { id, name, script, file },
                        danger: true,
                        alt: None,
                    },
                    RestoreTarget::Volume { name, used_by } => {
                        let mut lines = vec![
                            format!("Se extraerá {} ({size}) en el volumen «{name}».", file.display()),
                            "s = sobrescribir archivos (los que sobren se quedan) · l = vaciar el volumen y restaurar".into(),
                        ];
                        if !used_by.is_empty() {
                            lines.push(format!("Detén antes los contenedores que lo usan: {}", used_by.join(", ")));
                        }
                        Confirm {
                            title: format!("Restaurar volumen «{name}»"),
                            lines,
                            action: ConfirmAction::RestoreVolume { name, file },
                            danger: true,
                            alt: Some(('l', "Vaciar y restaurar".into())),
                        }
                    }
                };
                Mode::Confirm(Box::new(confirm))
            }
            PromptAction::DeployName { template, image } => {
                if !valid_name(&text) {
                    self.toast("Nombre inválido: usa letras, números, - _ . (máx. 63)", Level::Err);
                    return Mode::Prompt(Box::new(Prompt {
                        title: p.title,
                        hint: p.hint,
                        input: p.input,
                        action: PromptAction::DeployName { template, image },
                    }));
                }
                let Some(t) = self.catalog.get(&template).cloned() else { return Mode::Normal };
                // el nombre y sus derivados no deben existir ya
                let names: HashSet<&str> = self.snap.containers.iter().map(|c| c.name.as_str()).collect();
                let probe = Plan::build(&t, &text, None, "no");
                for i in 0..probe.services.len() {
                    let n = probe.container_name(i);
                    if names.contains(n.as_str()) {
                        self.toast(format!("Ya existe un contenedor llamado «{n}»"), Level::Err);
                        return Mode::Prompt(Box::new(Prompt {
                            title: p.title,
                            hint: p.hint,
                            input: p.input,
                            action: PromptAction::DeployName { template, image },
                        }));
                    }
                }
                let mut plan = Plan::build(&t, &text, self.cfg.tz.as_deref(), &self.cfg.restart());
                if let Some(img) = image {
                    plan.services[0].image = img;
                }
                let mut f = Form::new(FormKind::Deploy, plan);
                self.check_ports(&mut f);
                Mode::Form(Box::new(f))
            }
        }
    }

    pub(super) fn key_confirm(&mut self, c: Box<Confirm>, key: KeyEvent) -> Mode {
        match key.code {
            KeyCode::Char(k) if c.alt.as_ref().map(|(a, _)| *a == k).unwrap_or(false) => {
                self.run_confirmed(c.action, true);
                Mode::Normal
            }
            KeyCode::Char('y') | KeyCode::Char('s') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.run_confirmed(c.action, false);
                Mode::Normal
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => Mode::Normal,
            _ => Mode::Confirm(c),
        }
    }

    pub(super) fn run_confirmed(&mut self, a: ConfirmAction, alt: bool) {
        let e = self.engine.clone();
        let u = self.shared.usage_now.clone();
        match a {
            a @ (ConfirmAction::RestoreSql { .. } | ConfirmAction::RestoreVolume { .. }) => self.start_restore(a, alt),
            ConfirmAction::PruneContainers => {
                self.progress = Some("Limpiando contenedores detenidos…".into());
                self.bg(async move {
                    let o = crate::ops::PruneOpts { containers: true, ..Default::default() };
                    let r = match crate::ops::prune(&e, &o).await {
                        Ok(p) => (format!("{} contenedores eliminados, {} liberados", p.containers, human_bytes(p.freed)), Level::Ok),
                        Err(err) => (format!("{err}"), Level::Err),
                    };
                    u.notify_one();
                    r
                });
            }
            ConfirmAction::PruneNetworks => {
                self.bg(async move {
                    let o = crate::ops::PruneOpts { networks: true, ..Default::default() };
                    match crate::ops::prune(&e, &o).await {
                        Ok(p) => (format!("{} redes eliminadas", p.networks), Level::Ok),
                        Err(err) => (format!("{err}"), Level::Err),
                    }
                });
            }
            ConfirmAction::PruneBuild => {
                self.progress = Some("Limpiando caché de construcción…".into());
                self.bg(async move {
                    let o = crate::ops::PruneOpts { build: true, ..Default::default() };
                    let r = match crate::ops::prune(&e, &o).await {
                        Ok(p) => (format!("Caché de construcción limpia, {} liberados", human_bytes(p.freed)), Level::Ok),
                        Err(err) => (format!("{err}"), Level::Err),
                    };
                    u.notify_one();
                    r
                });
            }
            ConfirmAction::RemoveContainer { id, name } => self.do_act(id, name, Act::Remove, "Eliminando"),
            ConfirmAction::KillContainer { id, name } => self.do_act(id, name, Act::Kill, "Matando"),
            ConfirmAction::UpdateContainer { id, name } => self.start_update(id, name),
            ConfirmAction::DownStack { name } => {
                self.progress = Some(format!("Eliminando stack {name}…"));
                self.bg(async move {
                    match e.down_stack(&name, false).await {
                        Ok(n) => (format!("Stack {name}: {n} contenedores eliminados"), Level::Ok),
                        Err(err) => (format!("{name}: {err}"), Level::Err),
                    }
                });
            }
            ConfirmAction::RemoveImage { id, label } => {
                self.bg(async move {
                    match e.remove_image(&id).await {
                        Ok(()) => (format!("Imagen eliminada: {label}"), Level::Ok),
                        Err(err) => (format!("{label}: {err}"), Level::Err),
                    }
                });
            }
            ConfirmAction::PruneImages => {
                self.progress = Some("Limpiando imágenes…".into());
                self.bg(async move {
                    let o = crate::ops::PruneOpts { images: true, ..Default::default() };
                    let r = match crate::ops::prune(&e, &o).await {
                        Ok(p) => (format!("{} imágenes eliminadas, {} liberados", p.images, human_bytes(p.freed)), Level::Ok),
                        Err(err) => (format!("{err}"), Level::Err),
                    };
                    u.notify_one();
                    r
                });
            }
            ConfirmAction::RemoveVolume { name } => {
                self.bg(async move {
                    match e.remove_volume(&name).await {
                        Ok(()) => (format!("Volumen eliminado: {name}"), Level::Ok),
                        Err(err) => (format!("{name}: {err}"), Level::Err),
                    }
                });
            }
            ConfirmAction::PruneVolumes { names } => {
                self.progress = Some("Limpiando volúmenes…".into());
                self.bg(async move {
                    let (ok, fail) = crate::ops::remove_volumes(&e, &names).await;
                    u.notify_one();
                    if fail == 0 {
                        (format!("{ok} volúmenes eliminados"), Level::Ok)
                    } else {
                        (format!("{ok} eliminados, {fail} con error"), Level::Warn)
                    }
                });
            }
        }
    }

    pub(super) fn key_form(&mut self, mut f: Box<Form>, key: KeyEvent) -> Mode {
        // edición en curso
        if f.edit.is_some() {
            match key.code {
                KeyCode::Enter => {
                    f.commit_edit();
                    self.check_ports(&mut f);
                }
                KeyCode::Esc => f.edit = None,
                _ => {
                    if let Some((inp, _)) = &mut f.edit {
                        inp.handle(key);
                    }
                }
            }
            return Mode::Form(f);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') => f.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => f.move_sel(1),
            KeyCode::PageUp => f.move_sel(-8),
            KeyCode::PageDown => f.move_sel(8),
            KeyCode::Home | KeyCode::Char('g') => f.first(),
            KeyCode::End | KeyCode::Char('G') => f.last(),
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char(' ') => f.activate(),
            KeyCode::Char('d') | KeyCode::Delete if !ctrl => {
                f.delete_current();
                self.check_ports(&mut f);
            }
            KeyCode::Char('a') => {
                // saltar a «añadir variable» del servicio actual
                let svc = match f.current() {
                    Some(Row::Image(i) | Row::Port(i, _) | Row::AddPort(i) | Row::Env(i, _) | Row::AddEnv(i) | Row::Vol(i, _) | Row::AddVol(i) | Row::Restart(i) | Row::Mem(i) | Row::Cpus(i) | Row::Cmd(i)) => i,
                    _ => 0,
                };
                if let Some(p) = f.rows.iter().position(|r| *r == Row::AddEnv(svc)) {
                    f.sel = p;
                    f.activate();
                }
            }
            KeyCode::F(5) | KeyCode::Char('D') | KeyCode::Char('S') => return self.form_apply(f),
            KeyCode::Char('s') if ctrl => return self.form_apply(f),
            _ => {}
        }
        Mode::Form(f)
    }

    pub(super) fn form_apply(&mut self, mut f: Box<Form>) -> Mode {
        self.check_ports(&mut f);
        if f.blocking() {
            f.msg = Some(("Corrige los errores marcados antes de continuar".into(), Level::Err));
            return Mode::Form(f);
        }
        let f = *f;
        match f.kind {
            FormKind::Deploy => self.start_deploy(f.plan),
            FormKind::Edit(src) => self.start_recreate(*src, f.plan),
        }
        Mode::Normal
    }
}
