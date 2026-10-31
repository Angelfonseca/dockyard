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

}

mod keys;
mod messages;
mod ops;
mod overlays;
mod ports;
mod state;
mod tabs;
mod tasks;
mod helpers;

pub use helpers::*;
