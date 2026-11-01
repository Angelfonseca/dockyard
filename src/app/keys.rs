//! Teclado: entrada general, pestañas, filtros y navegación.

use super::*;

impl App {
    // ───────────────────────────── Teclado ─────────────────────────────

    pub fn on_paste(&mut self, s: &str) {
        match &mut self.mode {
            Mode::Prompt(p) => p.input.insert_str(s),
            Mode::Form(f) => {
                if let Some((inp, _)) = &mut f.edit {
                    inp.insert_str(s);
                }
            }
            Mode::Logs(lv) => {
                if let Some(i) = &mut lv.filter_input {
                    i.insert_str(s);
                }
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = match mode {
            Mode::Normal => {
                self.key_normal(key);
                return;
            }
            Mode::Logs(lv) => self.key_logs(lv, key),
            Mode::Text(tv) => self.key_text(tv, key),
            Mode::Prompt(p) => self.key_prompt(p, key),
            Mode::Confirm(c) => self.key_confirm(c, key),
            Mode::Form(f) => self.key_form(f, key),
        };
        self.set_extra();
    }

    pub(super) fn key_normal(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('1') => return self.goto(Tab::Panel),
            KeyCode::Char('2') => return self.goto(Tab::Containers),
            KeyCode::Char('3') => return self.goto(Tab::Images),
            KeyCode::Char('4') => return self.goto(Tab::Volumes),
            KeyCode::Char('5') => return self.goto(Tab::Catalog),
            KeyCode::Char('6') => return self.goto(Tab::Space),
            KeyCode::Char('X') => {
                self.pending = Some(Pending::HostShell);
                return;
            }
            KeyCode::Char('!') => {
                let where_ = self.engine.ssh_target().map(|(t, _)| t).unwrap_or_else(|| "esta máquina".into());
                self.hist_pos = None;
                self.mode = Mode::Prompt(Box::new(Prompt {
                    title: format!("Comando en {where_}"),
                    hint: "se ejecuta con sh -c · salida en vivo · ↑↓ historial · Esc en la salida lo detiene".into(),
                    input: LineInput::new(""),
                    action: PromptAction::HostExec,
                }));
                return;
            }
            KeyCode::Tab | KeyCode::Char(']') => {
                let i = (self.tab.index() + 1) % Tab::ALL.len();
                return self.goto(Tab::ALL[i]);
            }
            KeyCode::BackTab | KeyCode::Char('[') => {
                let i = (self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len();
                return self.goto(Tab::ALL[i]);
            }
            KeyCode::F(5) => {
                self.refresh_now();
                self.refresh_usage();
                self.toast("Actualizando…", Level::Info);
                return;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.mode = Mode::Text(Box::new(TextView {
                    title: "Ayuda".into(),
                    lines: help_lines(),
                    scroll: 0,
                    hscroll: 0,
                }));
                return;
            }
            _ => {}
        }
        let _ = shift;
        match self.tab {
            Tab::Panel => {
                if key.code == KeyCode::Enter {
                    self.goto(Tab::Containers);
                }
            }
            Tab::Containers => self.key_containers(key),
            Tab::Images => self.key_images(key),
            Tab::Volumes => self.key_volumes(key),
            Tab::Catalog => self.key_catalog(key),
            Tab::Space => self.key_space(key),
        }
    }

    pub(super) fn key_space(&mut self, key: KeyEvent) {
        let u = self.usage.clone().unwrap_or_default();
        let (title, lines, action) = match key.code {
            KeyCode::Char('c') => (
                "Limpiar contenedores detenidos",
                vec![
                    format!("Se eliminarán los {} contenedores detenidos (≈ {}).", u.containers.count.saturating_sub(u.containers.active), human_bytes(u.containers.reclaimable)),
                    "Sus volúmenes se conservan.".into(),
                ],
                ConfirmAction::PruneContainers,
            ),
            KeyCode::Char('i') => (
                "Limpiar imágenes sin uso",
                vec![format!("Se eliminarán las imágenes que ningún contenedor usa (≈ {}).", human_bytes(u.images.reclaimable)), "Se vuelven a descargar si las necesitas.".into()],
                ConfirmAction::PruneImages,
            ),
            KeyCode::Char('b') => (
                "Limpiar caché de construcción",
                vec![format!("Se borrará toda la caché de builds sin uso (≈ {}).", if u.build.known { human_bytes(u.build.reclaimable) } else { "n/d".into() }), "La próxima construcción tardará más.".into()],
                ConfirmAction::PruneBuild,
            ),
            KeyCode::Char('n') => ("Limpiar redes sin uso", vec!["Se eliminarán las redes que ningún contenedor usa.".into()], ConfirmAction::PruneNetworks),
            KeyCode::Char('v') => {
                let names: Vec<String> = self.volumes.iter().filter(|v| v.used_by.is_empty()).map(|v| v.name.clone()).collect();
                if names.is_empty() {
                    self.toast("No hay volúmenes huérfanos", Level::Info);
                    return;
                }
                let size: u64 = names.iter().filter_map(|n| u.vol_sizes.get(n)).sum();
                let mut lines = vec![format!("Se borrarán {} volúmenes huérfanos (≈ {}) y SUS DATOS:", names.len(), human_bytes(size))];
                for n in names.iter().take(8) {
                    lines.push(format!("  • {}", truncate(n, 60)));
                }
                if names.len() > 8 {
                    lines.push(format!("  … y {} más", names.len() - 8));
                }
                ("Limpiar volúmenes huérfanos", lines, ConfirmAction::PruneVolumes { names })
            }
            KeyCode::Char('r') => {
                self.refresh_usage();
                self.toast("Recalculando uso de disco…", Level::Info);
                return;
            }
            _ => return,
        };
        self.mode = Mode::Confirm(Box::new(Confirm { title: title.into(), lines, action, danger: true, alt: None }));
    }

    pub(super) fn nav(&mut self, key: KeyEvent, which: Tab) -> bool {
        let (keys, page): (Vec<String>, isize) = match which {
            Tab::Containers => (self.visible_containers().iter().map(|c| c.id.clone()).collect(), 10),
            Tab::Images => (self.visible_images().iter().map(|c| c.id.clone()).collect(), 10),
            Tab::Volumes => (self.visible_volumes().iter().map(|c| c.name.clone()).collect(), 10),
            Tab::Catalog => (self.visible_templates().iter().map(|c| c.id.clone()).collect(), 10),
            Tab::Panel | Tab::Space => return false,
        };
        let sel = match which {
            Tab::Containers => &mut self.c_sel,
            Tab::Images => &mut self.i_sel,
            Tab::Volumes => &mut self.v_sel,
            _ => &mut self.cat_sel,
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => sel.mv(-1, &keys),
            KeyCode::Down | KeyCode::Char('j') => sel.mv(1, &keys),
            KeyCode::PageUp => sel.mv(-page, &keys),
            KeyCode::PageDown => sel.mv(page, &keys),
            KeyCode::Home | KeyCode::Char('g') => sel.to(0, &keys),
            KeyCode::End | KeyCode::Char('G') => sel.to(usize::MAX, &keys),
            _ => return false,
        }
        true
    }

    pub(super) fn open_filter(&mut self, tab: Tab) {
        let cur = match tab {
            Tab::Containers => self.c_filter.clone(),
            Tab::Images => self.i_filter.clone(),
            Tab::Volumes => self.v_filter.clone(),
            _ => self.cat_filter.clone(),
        };
        self.mode = Mode::Prompt(Box::new(Prompt {
            title: "Filtrar".into(),
            hint: "texto a buscar · Enter aplica · vacío limpia".into(),
            input: LineInput::new(&cur),
            action: PromptAction::Filter(tab),
        }));
    }

    pub(super) fn clear_filter(&mut self, tab: Tab) -> bool {
        let f = match tab {
            Tab::Containers => &mut self.c_filter,
            Tab::Images => &mut self.i_filter,
            Tab::Volumes => &mut self.v_filter,
            _ => &mut self.cat_filter,
        };
        if f.is_empty() {
            false
        } else {
            f.clear();
            true
        }
    }

}
