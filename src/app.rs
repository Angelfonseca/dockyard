//! Estado de la TUI y manejo de teclado.

use crate::catalog::{Catalog, Plan, PortSpec, Template, valid_name};
use crate::collector::Shared;
use crate::config::Config;
use crate::engine::{Act, Engine, UpdateOutcome};
use crate::forms::{Form, FormKind, Row};
use crate::input::LineInput;
use crate::model::*;
use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

pub const HIST: usize = 90;
pub const LOG_CAP: usize = 8000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Panel,
    Containers,
    Images,
    Volumes,
    Catalog,
    Space,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Panel, Tab::Containers, Tab::Images, Tab::Volumes, Tab::Catalog, Tab::Space];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Panel => "Panel",
            Tab::Containers => "Contenedores",
            Tab::Images => "Imágenes",
            Tab::Volumes => "Volúmenes",
            Tab::Catalog => "Catálogo",
            Tab::Space => "Espacio",
        }
    }
    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    Name,
    Cpu,
    Mem,
    State,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "nombre",
            Sort::Cpu => "CPU",
            Sort::Mem => "memoria",
            Sort::State => "estado",
        }
    }
    fn next(self) -> Sort {
        match self {
            Sort::Name => Sort::Cpu,
            Sort::Cpu => Sort::Mem,
            Sort::Mem => Sort::State,
            Sort::State => Sort::Name,
        }
    }
}

#[derive(Default)]
pub struct ListSel {
    pub idx: usize,
    pub key: Option<String>,
    pub ts: TableState,
}

impl ListSel {
    pub fn sync(&mut self, keys: &[String]) {
        if keys.is_empty() {
            self.idx = 0;
            self.key = None;
            self.ts.select(None);
            return;
        }
        if let Some(k) = &self.key {
            if let Some(p) = keys.iter().position(|x| x == k) {
                self.idx = p;
                self.ts.select(Some(p));
                return;
            }
        }
        self.idx = self.idx.min(keys.len() - 1);
        self.key = Some(keys[self.idx].clone());
        self.ts.select(Some(self.idx));
    }

    pub fn mv(&mut self, d: isize, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let i = (self.idx as isize + d).clamp(0, keys.len() as isize - 1) as usize;
        self.idx = i;
        self.key = Some(keys[i].clone());
        self.ts.select(Some(i));
    }

    pub fn to(&mut self, i: usize, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let i = i.min(keys.len() - 1);
        self.idx = i;
        self.key = Some(keys[i].clone());
        self.ts.select(Some(i));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogKind {
    /// Logs de un contenedor (se puede reiniciar con marcas de tiempo).
    Container,
    /// Salida de un comando o tarea (exec, host, compose, git).
    Command,
}

pub struct LogView {
    pub kind: LogKind,
    pub id: String,
    pub name: String,
    pub session: u64,
    pub lines: VecDeque<String>,
    pub follow: bool,
    pub scroll: usize,
    pub hscroll: usize,
    pub filter: String,
    pub filter_input: Option<LineInput>,
    pub wrap: bool,
    pub timestamps: bool,
    pub ended: Option<String>,
    pub ended_ok: bool,
    pub total: u64,
    pub handle: Option<JoinHandle<()>>,
}

impl Drop for LogView {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

pub struct TextView {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub hscroll: usize,
}

pub enum PromptAction {
    Pull,
    Exec { id: String, name: String },
    HostExec,
    DeployName { template: String, image: Option<String> },
    Filter(Tab),
    /// Restaurar en un contenedor (SQL) o en un volumen (.tar.gz); el texto es la ruta del archivo.
    Restore { target: RestoreTarget },
    /// Archivo a editar; `volume` = Some si es dentro de un volumen.
    EditFile { id: String, name: String, volume: bool },
    CloneVolume { src: String },
    Git,
    Compose,
}

#[derive(Clone)]
pub enum RestoreTarget {
    Sql { id: String, name: String, script: String },
    Volume { name: String, used_by: Vec<String> },
}

pub struct Prompt {
    pub title: String,
    pub hint: String,
    pub input: LineInput,
    pub action: PromptAction,
}

pub enum ConfirmAction {
    RemoveContainer { id: String, name: String },
    KillContainer { id: String, name: String },
    UpdateContainer { id: String, name: String },
    DownStack { name: String },
    RemoveImage { id: String, label: String },
    PruneImages,
    RemoveVolume { name: String },
    PruneVolumes { names: Vec<String> },
    PruneContainers,
    PruneNetworks,
    PruneBuild,
    RestoreSql { id: String, name: String, script: String, file: std::path::PathBuf },
    /// `alt` en la confirmación = vaciar el volumen antes
    RestoreVolume { name: String, file: std::path::PathBuf },
}

pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    pub action: ConfirmAction,
    pub danger: bool,
    /// Segunda opción (tecla, texto), ej. ('l', "vaciar y restaurar").
    pub alt: Option<(char, String)>,
}

pub enum Mode {
    Normal,
    Logs(Box<LogView>),
    Text(Box<TextView>),
    Prompt(Box<Prompt>),
    Confirm(Box<Confirm>),
    Form(Box<Form>),
}

/// Acciones que necesitan la terminal completa (se suspende la TUI).
pub enum Pending {
    Shell { id: String, name: String },
    HostShell,
    /// Editar un archivo: `id` es el contenedor, o el volumen si `volume`.
    EditFile { id: String, name: String, path: String, volume: bool },
}

pub struct History {
    pub cpu: VecDeque<u64>,
    pub mem: VecDeque<u64>,
    pub rx: VecDeque<u64>,
    pub tx: VecDeque<u64>,
}

pub struct App {
    pub engine: Arc<Engine>,
    pub tx: Tx,
    pub shared: Arc<Shared>,
    pub cfg: Config,
    pub catalog: Catalog,
    pub tab: Tab,
    pub mode: Mode,
    pub snap: Snapshot,
    pub images: Vec<IRow>,
    pub volumes: Vec<VRow>,
    pub connected: bool,
    pub conn_err: Option<String>,
    pub hist: History,
    pub chist: HashMap<String, (VecDeque<u64>, VecDeque<u64>)>,
    pub c_sel: ListSel,
    pub i_sel: ListSel,
    pub v_sel: ListSel,
    pub cat_sel: ListSel,
    pub c_filter: String,
    pub i_filter: String,
    pub v_filter: String,
    pub cat_filter: String,
    pub show_all: bool,
    pub sort: Sort,
    pub toast: Option<(String, Level, Instant)>,
    pub progress: Option<String>,
    pub should_quit: bool,
    pub pending: Option<Pending>,
    pub log_session: u64,
    pub frame: u64,
    pub started: Instant,
    pub usage: Option<DiskUsage>,
    /// Historial de comandos por contenedor («» = host).
    pub cmd_hist: HashMap<String, Vec<String>>,
    pub hist_pos: Option<usize>,
}

impl App {
    pub fn new(engine: Arc<Engine>, tx: Tx, shared: Arc<Shared>, cfg: Config, catalog: Catalog) -> App {
        let h = || VecDeque::with_capacity(HIST);
        App {
            engine,
            tx,
            shared,
            cfg,
            catalog,
            tab: Tab::Panel,
            mode: Mode::Normal,
            snap: Snapshot::default(),
            images: vec![],
            volumes: vec![],
            connected: false,
            conn_err: None,
            hist: History { cpu: h(), mem: h(), rx: h(), tx: h() },
            chist: HashMap::new(),
            c_sel: ListSel::default(),
            i_sel: ListSel::default(),
            v_sel: ListSel::default(),
            cat_sel: ListSel::default(),
            c_filter: String::new(),
            i_filter: String::new(),
            v_filter: String::new(),
            cat_filter: String::new(),
            show_all: true,
            sort: Sort::Name,
            toast: None,
            progress: None,
            should_quit: false,
            pending: None,
            log_session: 0,
            frame: 0,
            started: Instant::now(),
            usage: None,
            cmd_hist: HashMap::new(),
            hist_pos: None,
        }
    }

    // ───────────────────────────── Utilidades de estado ─────────────────────────────

    pub fn toast(&mut self, msg: impl Into<String>, lvl: Level) {
        self.toast = Some((msg.into(), lvl, Instant::now()));
    }

    pub fn refresh_now(&self) {
        self.shared.notify.notify_one();
    }

    fn set_extra(&self) {
        let want = matches!(self.tab, Tab::Images | Tab::Volumes | Tab::Containers | Tab::Space) || matches!(self.mode, Mode::Form(_));
        self.shared.want_extra.store(want, Ordering::Relaxed);
        self.shared.want_usage.store(matches!(self.tab, Tab::Volumes | Tab::Space), Ordering::Relaxed);
    }

    pub fn goto(&mut self, t: Tab) {
        self.tab = t;
        self.set_extra();
        if matches!(t, Tab::Images | Tab::Volumes | Tab::Space) {
            self.refresh_now();
        }
    }

    /// Recalcular el uso de disco ya (tras borrar algo).
    fn refresh_usage(&self) {
        self.shared.usage_now.notify_one();
    }

    fn push_hist(q: &mut VecDeque<u64>, v: u64) {
        if q.len() >= HIST {
            q.pop_front();
        }
        q.push_back(v);
    }

    pub fn apply_snapshot(&mut self, mut s: Snapshot) {
        if let Some(i) = s.images.take() {
            self.images = i;
        }
        if let Some(v) = s.volumes.take() {
            self.volumes = v;
        }
        let h = &s.host;
        Self::push_hist(&mut self.hist.cpu, h.cpu.round().clamp(0.0, 100.0) as u64);
        let mp = if h.mem_total > 0 { h.mem_used * 100 / h.mem_total } else { 0 };
        Self::push_hist(&mut self.hist.mem, mp);
        Self::push_hist(&mut self.hist.rx, h.net_rx_rate as u64);
        Self::push_hist(&mut self.hist.tx, h.net_tx_rate as u64);
        for c in &s.containers {
            if let Some(st) = s.stats.get(&c.id) {
                let e = self.chist.entry(c.id.clone()).or_default();
                Self::push_hist(&mut e.0, st.cpu.round().max(0.0) as u64);
                let lim = if st.mem_limit > 0 { st.mem_limit } else { s.host.mem_total.max(1) };
                Self::push_hist(&mut e.1, st.mem * 100 / lim.max(1));
            }
        }
        let ids: HashSet<&String> = s.containers.iter().map(|c| &c.id).collect();
        self.chist.retain(|k, _| ids.contains(k));
        self.snap = s;
        self.connected = true;
        self.conn_err = None;
    }

    pub fn visible_containers(&self) -> Vec<&CRow> {
        let f = self.c_filter.to_lowercase();
        let mut v: Vec<&CRow> = self
            .snap
            .containers
            .iter()
            .filter(|c| self.show_all || c.is_up())
            .filter(|c| {
                f.is_empty()
                    || c.name.to_lowercase().contains(&f)
                    || c.image.to_lowercase().contains(&f)
                    || c.stack.as_deref().map(|s| s.to_lowercase().contains(&f)).unwrap_or(false)
                    || c.state.label().contains(&f)
            })
            .collect();
        let stats = &self.snap.stats;
        match self.sort {
            Sort::Name => v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            Sort::Cpu => v.sort_by(|a, b| {
                let ca = stats.get(&a.id).map(|s| s.cpu).unwrap_or(-1.0);
                let cb = stats.get(&b.id).map(|s| s.cpu).unwrap_or(-1.0);
                cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Mem => v.sort_by(|a, b| {
                let ma = stats.get(&a.id).map(|s| s.mem).unwrap_or(0);
                let mb = stats.get(&b.id).map(|s| s.mem).unwrap_or(0);
                mb.cmp(&ma)
            }),
            Sort::State => v.sort_by(|a, b| {
                (!a.is_up(), a.name.to_lowercase()).cmp(&(!b.is_up(), b.name.to_lowercase()))
            }),
        }
        v
    }

    pub fn visible_images(&self) -> Vec<&IRow> {
        let f = self.i_filter.to_lowercase();
        self.images
            .iter()
            .filter(|i| f.is_empty() || i.tags.iter().any(|t| t.to_lowercase().contains(&f)) || i.id.contains(&f))
            .collect()
    }

    pub fn visible_volumes(&self) -> Vec<&VRow> {
        let f = self.v_filter.to_lowercase();
        self.volumes.iter().filter(|v| f.is_empty() || v.name.to_lowercase().contains(&f)).collect()
    }

    pub fn visible_templates(&self) -> Vec<&Template> {
        let f = self.cat_filter.to_lowercase();
        self.catalog
            .templates
            .iter()
            .filter(|t| {
                f.is_empty()
                    || t.id.contains(&f)
                    || t.name.to_lowercase().contains(&f)
                    || t.category.to_lowercase().contains(&f)
                    || t.description.to_lowercase().contains(&f)
            })
            .collect()
    }

    pub fn sync_selections(&mut self) {
        let ck: Vec<String> = self.visible_containers().iter().map(|c| c.id.clone()).collect();
        self.c_sel.sync(&ck);
        let ik: Vec<String> = self.visible_images().iter().map(|c| c.id.clone()).collect();
        self.i_sel.sync(&ik);
        let vk: Vec<String> = self.visible_volumes().iter().map(|c| c.name.clone()).collect();
        self.v_sel.sync(&vk);
        let tk: Vec<String> = self.visible_templates().iter().map(|c| c.id.clone()).collect();
        self.cat_sel.sync(&tk);
    }

    pub fn cur_container(&self) -> Option<&CRow> {
        let k = self.c_sel.key.as_ref()?;
        self.snap.containers.iter().find(|c| &c.id == k)
    }

    pub fn cur_image(&self) -> Option<&IRow> {
        let k = self.i_sel.key.as_ref()?;
        self.images.iter().find(|c| &c.id == k)
    }

    pub fn cur_volume(&self) -> Option<&VRow> {
        let k = self.v_sel.key.as_ref()?;
        self.volumes.iter().find(|c| &c.name == k)
    }

    pub fn cur_template(&self) -> Option<&Template> {
        let k = self.cat_sel.key.as_ref()?;
        self.catalog.get(k)
    }

    // ───────────────────────────── Mensajes de tareas ─────────────────────────────

    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Snapshot(s) => self.apply_snapshot(*s),
            Msg::Disconnected(e) => {
                self.connected = false;
                self.conn_err = Some(e);
            }
            Msg::Toast(s, l) => {
                self.progress = None;
                self.toast(s, l);
            }
            Msg::Progress(s) => self.progress = Some(s),
            Msg::Text { title, body } => {
                self.progress = None;
                self.mode = Mode::Text(Box::new(TextView {
                    title,
                    lines: body.lines().map(|l| l.to_string()).collect(),
                    scroll: 0,
                    hscroll: 0,
                }));
            }
            Msg::Log { session, lines } => {
                if let Mode::Logs(lv) = &mut self.mode {
                    if lv.session == session {
                        lv.total += lines.len() as u64;
                        for l in lines {
                            if lv.lines.len() >= LOG_CAP {
                                lv.lines.pop_front();
                                if !lv.follow {
                                    lv.scroll = lv.scroll.saturating_sub(1);
                                }
                            }
                            lv.lines.push_back(l);
                        }
                    }
                }
            }
            Msg::LogEnd { session, msg, ok } => {
                if let Mode::Logs(lv) = &mut self.mode {
                    if lv.session == session {
                        lv.ended = Some(msg);
                        lv.ended_ok = ok;
                    }
                }
                self.refresh_now();
            }
            Msg::EditLoaded(src, plan) => {
                self.progress = None;
                let mut f = Form::new(FormKind::Edit(src), *plan);
                self.check_ports(&mut f);
                self.mode = Mode::Form(Box::new(f));
                self.set_extra();
            }
            Msg::Usage(u) => self.usage = Some(*u),
            Msg::DeployForm(plan) => {
                self.progress = None;
                let mut f = Form::new(FormKind::Deploy, *plan);
                self.check_ports(&mut f);
                self.mode = Mode::Form(Box::new(f));
                self.set_extra();
            }
        }
    }

    // ───────────────────────────── Tareas en segundo plano ─────────────────────────────

    /// Ejecuta una acción y publica su resultado como aviso; luego refresca.
    fn bg<F>(&self, fut: F)
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

    fn do_act(&mut self, id: String, name: String, act: Act, verb: &'static str) {
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
    fn run_exec(&mut self, id: String, name: String, line: String) {
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
    fn run_host(&mut self, line: String) {
        self.remember_cmd("", &line);
        let e = self.engine.clone();
        let where_ = e.ssh_target().map(|(t, _)| t).unwrap_or_else(|| "host".into());
        let lv = self.start_task_view(format!("{where_} $ {line}"), move |mut out| async move {
            crate::ext::run_lines(crate::ext::host_command(&e, &line), "el comando", &mut out).await?;
            Ok("✔ terminó (código 0)".into())
        });
        self.mode = Mode::Logs(Box::new(lv));
    }

    fn remember_cmd(&mut self, key: &str, line: &str) {
        let h = self.cmd_hist.entry(key.to_string()).or_default();
        h.retain(|x| x != line);
        h.push(line.to_string());
        if h.len() > 50 {
            h.remove(0);
        }
    }

    fn start_compose(&mut self, path: String) {
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
    fn start_git(&mut self, spec: String) {
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
            let src = crate::ext::GitSource { url, git_ref, name, subdir: None, auth };
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

    fn open_text_task<F>(&mut self, busy: String, title: String, fut: F)
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

    fn open_creds(&mut self, target: String) {
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

    fn open_volume_du(&mut self, name: String) {
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

    fn start_clone_volume(&mut self, src: String, dst: String) {
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

    fn start_restore(&mut self, a: ConfirmAction, clean: bool) {
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

    fn open_inspect(&mut self, id: String, name: String) {
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

    fn open_edit(&mut self, id: String, name: String) {
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

    fn start_backup_container(&mut self, c: &CRow) {
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

    fn start_backup_volume(&mut self, name: String) {
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

    fn start_pull(&mut self, image: String) {
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

    fn start_update(&mut self, id: String, name: String) {
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

    fn start_deploy(&mut self, plan: Plan) {
        self.progress = Some(format!("Desplegando {}…", plan.name));
        let e = self.engine.clone();
        let tx = self.tx.clone();
        let n = self.shared.notify.clone();
        tokio::spawn(async move {
            let txp = tx.clone();
            let mut prog = move |s: String| {
                let _ = txp.send(Msg::Progress(s));
            };
            match e.deploy(&plan, &mut prog).await {
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

    fn start_recreate(&mut self, src: crate::engine::EditSource, plan: Plan) {
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

    // ───────────────────────────── Comprobación de puertos ─────────────────────────────

    pub fn check_ports(&self, f: &mut Form) {
        let own_id = if let FormKind::Edit(src) = &f.kind { Some(src.id.clone()) } else { None };
        let mut others: HashSet<(u16, String)> = HashSet::new();
        let mut own: HashSet<(u16, String)> = HashSet::new();
        for c in &self.snap.containers {
            for p in &c.published {
                if Some(&c.id) == own_id.as_ref() {
                    own.insert(p.clone());
                } else {
                    others.insert(p.clone());
                }
            }
        }
        let local = std::env::var("DOCKER_HOST").map(|h| h.starts_with("unix://")).unwrap_or(true);
        let mut issues = Vec::new();
        for s in &f.plan.services {
            for p in &s.ports {
                if let Ok(ps) = PortSpec::parse(p) {
                    if let Some(h) = ps.host {
                        let key = (h, ps.proto.clone());
                        let taken = others.contains(&key) || (local && !own.contains(&key) && !can_bind(h, &ps.proto));
                        if taken && !issues.contains(&h.to_string()) {
                            issues.push(h.to_string());
                        }
                    }
                }
            }
        }
        f.port_issues = issues;
    }

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

    fn key_normal(&mut self, key: KeyEvent) {
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

    fn key_space(&mut self, key: KeyEvent) {
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

    fn nav(&mut self, key: KeyEvent, which: Tab) -> bool {
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

    fn open_filter(&mut self, tab: Tab) {
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

    fn clear_filter(&mut self, tab: Tab) -> bool {
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

    fn key_containers(&mut self, key: KeyEvent) {
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

    fn key_images(&mut self, key: KeyEvent) {
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

    fn key_volumes(&mut self, key: KeyEvent) {
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

    fn key_catalog(&mut self, key: KeyEvent) {
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

    fn key_text(&mut self, mut tv: Box<TextView>, key: KeyEvent) -> Mode {
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

    fn key_logs(&mut self, mut lv: Box<LogView>, key: KeyEvent) -> Mode {
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

    fn key_prompt(&mut self, mut p: Box<Prompt>, key: KeyEvent) -> Mode {
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

    fn submit_prompt(&mut self, p: Prompt) -> Mode {
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

    fn key_confirm(&mut self, c: Box<Confirm>, key: KeyEvent) -> Mode {
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

    fn run_confirmed(&mut self, a: ConfirmAction, alt: bool) {
        let e = self.engine.clone();
        let u = self.shared.usage_now.clone();
        match a {
            a @ (ConfirmAction::RestoreSql { .. } | ConfirmAction::RestoreVolume { .. }) => self.start_restore(a, alt),
            ConfirmAction::PruneContainers => {
                self.progress = Some("Limpiando contenedores detenidos…".into());
                self.bg(async move {
                    let r = match e.prune_containers(None).await {
                        Ok((n, b)) => (format!("{n} contenedores eliminados, {} liberados", human_bytes(b)), Level::Ok),
                        Err(err) => (format!("{err}"), Level::Err),
                    };
                    u.notify_one();
                    r
                });
            }
            ConfirmAction::PruneNetworks => {
                self.bg(async move {
                    match e.prune_networks(None).await {
                        Ok(n) => (format!("{n} redes eliminadas"), Level::Ok),
                        Err(err) => (format!("{err}"), Level::Err),
                    }
                });
            }
            ConfirmAction::PruneBuild => {
                self.progress = Some("Limpiando caché de construcción…".into());
                self.bg(async move {
                    let r = match e.prune_build(None).await {
                        Ok(b) => (format!("Caché de construcción limpia, {} liberados", human_bytes(b)), Level::Ok),
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
                    let r = match e.prune_images(None).await {
                        Ok((n, b)) => (format!("{n} imágenes eliminadas, {} liberados", human_bytes(b)), Level::Ok),
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
                    let (mut ok, mut fail) = (0, 0);
                    for n in names {
                        if e.remove_volume(&n).await.is_ok() {
                            ok += 1;
                        } else {
                            fail += 1;
                        }
                    }
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

    fn key_form(&mut self, mut f: Box<Form>, key: KeyEvent) -> Mode {
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

    fn form_apply(&mut self, mut f: Box<Form>) -> Mode {
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

// ───────────────────────────── Auxiliares ─────────────────────────────

pub fn filtered_len(lv: &LogView) -> usize {
    if lv.filter.is_empty() {
        lv.lines.len()
    } else {
        let f = lv.filter.to_lowercase();
        lv.lines.iter().filter(|l| l.to_lowercase().contains(&f)).count()
    }
}

/// Expande «~» al inicio de una ruta escrita a mano.
pub fn shell_path(p: &str) -> String {
    let p = p.trim();
    if p == "~" {
        home_dir().display().to_string()
    } else if let Some(r) = p.strip_prefix("~/") {
        home_dir().join(r).display().to_string()
    } else {
        p.to_string()
    }
}

pub fn suggest_name(existing: &[CRow], base: &str) -> String {
    let base: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    let taken = |n: &str| existing.iter().any(|c| c.name == n || c.name.starts_with(&format!("{n}-")));
    if !taken(&base) {
        return base;
    }
    for i in 2..100 {
        let n = format!("{base}-{i}");
        if !taken(&n) {
            return n;
        }
    }
    base
}

/// ¿Se puede enlazar el puerto localmente? (permiso denegado cuenta como «no sé» = libre)
pub fn can_bind(port: u16, proto: &str) -> bool {
    use std::io::ErrorKind;
    let r = if proto == "udp" {
        std::net::UdpSocket::bind(("0.0.0.0", port)).map(|_| ())
    } else {
        std::net::TcpListener::bind(("0.0.0.0", port)).map(|_| ())
    };
    match r {
        Ok(()) => true,
        Err(e) => e.kind() != ErrorKind::AddrInUse,
    }
}

pub fn help_lines() -> Vec<String> {
    let t = |s: &str| s.to_string();
    vec![
        t("  dockyard — gestor de Docker en terminal"),
        t(""),
        t("  GLOBAL"),
        t("    1-6 / Tab / [ ]    cambiar de pestaña"),
        t("    X                  shell de la máquina (o del servidor si Docker es remoto por SSH)"),
        t("    !                  comando en la máquina, con salida en vivo"),
        t("    F5                 actualizar ahora"),
        t("    ?                  esta ayuda        q  salir"),
        t(""),
        t("  CONTENEDORES"),
        t("    ↑↓ jk  PgUp/PgDn   moverse            g/G  inicio/fin"),
        t("    Enter / l          ver logs en vivo"),
        t("    s                  iniciar / detener"),
        t("    r                  reiniciar          p  pausar/reanudar"),
        t("    e                  editar (variables, puertos, volúmenes, imagen, RAM, CPU…) y recrear"),
        t("    u                  actualizar (imagen nueva, git pull + build, o compose pull)"),
        t("    x                  shell interactiva (bash/sh) — no necesita el CLI de docker"),
        t("    c                  ejecutar un comando con salida en vivo (↑↓ historial)"),
        t("    F                  editar un archivo con tu $EDITOR (también si está detenido)"),
        t("    i                  inspeccionar (JSON completo)"),
        t("    b                  respaldo de base de datos (pg_dump, mariadb-dump…)"),
        t("    R                  restaurar un respaldo de base de datos"),
        t("    C                  ver contraseñas y tokens del contenedor o su stack"),
        t("    d                  eliminar            D  eliminar todo el stack (también de Compose)"),
        t("    K                  matar (SIGKILL)"),
        t("    a                  mostrar todos / solo activos"),
        t("    o                  cambiar orden       /  filtrar    Esc  limpiar filtro"),
        t("    n                  ir al catálogo para instalar algo nuevo"),
        t(""),
        t("  LOGS Y SALIDA DE COMANDOS"),
        t("    f / Espacio        seguir / pausar     /  filtrar"),
        t("    ↑↓ PgUp PgDn g G   desplazarse         ←→  desplazamiento horizontal"),
        t("    w                  ajustar líneas      t  marcas de tiempo (solo logs)"),
        t("    c                  limpiar pantalla    Esc  cerrar (detiene el comando si sigue)"),
        t(""),
        t("  IMÁGENES"),
        t("    p                  descargar imagen    Enter  crear contenedor desde ella"),
        t("    d                  eliminar            D  limpiar las que nadie usa"),
        t(""),
        t("  VOLÚMENES"),
        t("    Enter              explorar: lo más pesado y su contenido"),
        t("    f                  editar un archivo del volumen"),
        t("    b                  respaldar a .tar.gz  R  restaurar (s sobrescribe, l vacía antes)"),
        t("    c                  clonar (copia exacta)"),
        t("    d                  eliminar             D  limpiar huérfanos"),
        t(""),
        t("  CATÁLOGO"),
        t("    Enter              instalar plantilla (bases de datos, apps, stacks…)"),
        t("    r                  traer desde un repositorio Git (URL[#rama]): compose.yaml o Dockerfile"),
        t("    o                  levantar un proyecto Compose (carpeta o archivo)"),
        t("    /                  buscar"),
        t(""),
        t("  ESPACIO"),
        t("    c i b n v          limpiar contenedores detenidos, imágenes sin uso, caché de build,"),
        t("                       redes sin uso o volúmenes huérfanos (siempre pide confirmación)"),
        t("    r                  recalcular"),
        t(""),
        t("  FORMULARIO"),
        t("    ↑↓                 moverse             Enter  editar / alternar"),
        t("    a                  añadir variable     d  borrar fila"),
        t("    F5 / D             desplegar / aplicar Esc  cancelar"),
        t(""),
        t("  Plantillas propias: ~/.config/dockyard/templates.toml"),
        t("  Ajustes: ~/.config/dockyard/config.toml (tz, backups_dir, refresh_secs, default_restart, log_max_size…)"),
    ]
}
