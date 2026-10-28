//! Tipos compartidos entre el recolector, la TUI y la CLI.

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum St {
    Running,
    Paused,
    Restarting,
    Created,
    Exited,
    Dead,
    Other,
}

impl St {
    pub fn label(self) -> &'static str {
        match self {
            St::Running => "activo",
            St::Paused => "pausado",
            St::Restarting => "reiniciando",
            St::Created => "creado",
            St::Exited => "detenido",
            St::Dead => "muerto",
            St::Other => "?",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    Healthy,
    Unhealthy,
    Starting,
}

#[derive(Clone, Debug)]
pub struct CRow {
    pub id: String,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub state: St,
    pub status: String,
    pub health: Option<Health>,
    pub ports: String,
    /// Puertos publicados en el host: (puerto, protocolo)
    pub published: Vec<(u16, String)>,
    pub stack: Option<String>,
    pub created: i64,
    /// Volúmenes nombrados o carpetas montadas.
    pub mounts: Vec<String>,
    pub networks: Vec<String>,
}

impl CRow {
    pub fn is_up(&self) -> bool {
        matches!(self.state, St::Running | St::Paused | St::Restarting)
    }
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // rx/tx totales: reservados para la vista de detalle
pub struct CStat {
    pub cpu: f64,
    pub mem: u64,
    pub mem_limit: u64,
    pub rx: u64,
    pub tx: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

#[derive(Clone, Debug)]
pub struct IRow {
    pub id: String,
    pub tags: Vec<String>,
    pub size: u64,
    pub created: i64,
    pub used_by: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct VRow {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub used_by: Vec<String>,
}

/// Uso de disco de una categoría (imágenes, contenedores, volúmenes, caché de build).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub count: u64,
    pub active: u64,
    pub size: u64,
    pub reclaimable: u64,
    /// false si el daemon no informó este dato (API vieja)
    pub known: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DiskUsage {
    pub images: Usage,
    pub containers: Usage,
    pub volumes: Usage,
    pub build: Usage,
    pub vol_sizes: HashMap<String, u64>,
    /// Carpeta de datos de Docker y el disco que la contiene (solo si es local): (ruta, usado, total)
    pub root: Option<(String, u64, u64)>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskInfo {
    pub mount: String,
    pub used: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // os/cpu_model: reservados para la cabecera del panel
pub struct Host {
    pub hostname: String,
    pub os: String,
    pub cpu: f32,
    pub cores: Vec<f32>,
    pub cpu_model: String,
    pub temp_c: Option<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: (f64, f64, f64),
    pub uptime: u64,
    pub disks: Vec<DiskInfo>,
    pub net_rx_rate: f64,
    pub net_tx_rate: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub containers: Vec<CRow>,
    pub stats: HashMap<String, CStat>,
    pub host: Host,
    pub images: Option<Vec<IRow>>,
    pub volumes: Option<Vec<VRow>>,
    pub docker_version: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Err,
}

pub type Tx = tokio::sync::mpsc::UnboundedSender<Msg>;

/// Mensajes que las tareas en segundo plano envían a la TUI.
pub enum Msg {
    Snapshot(Box<Snapshot>),
    Disconnected(String),
    Log { session: u64, lines: Vec<String> },
    /// Fin de una vista de logs/comando: mensaje final y si terminó bien.
    LogEnd { session: u64, msg: String, ok: bool },
    Toast(String, Level),
    Progress(String),
    /// Abre una vista de texto (resultado de comandos, credenciales, etc.)
    Text { title: String, body: String },
    /// Resultado de cargar un contenedor para editarlo.
    EditLoaded(Box<crate::engine::EditSource>, Box<crate::catalog::Plan>),
    /// Uso de disco (system df), calculado aparte porque puede tardar.
    Usage(Box<DiskUsage>),
    /// Abre el formulario de despliegue con un plan ya armado (imagen construida desde Git).
    DeployForm(Box<crate::catalog::Plan>),
}
