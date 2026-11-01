//! Capa de acceso a Docker (bollard): listados, acciones, despliegue, edición, respaldos.

use crate::catalog::{Plan, PortSpec, SvcPlan};
use crate::model::*;
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::models::*;
use bollard::query_parameters::*;
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type Prog<'a> = &'a mut (dyn FnMut(String) + Send);

const HELPER: &str = "alpine:3";

pub struct Engine {
    pub d: Docker,
    /// Dirección del daemon (unix:///…, tcp://…, ssh://…): se muestra y se pasa a `docker compose`/`docker build`.
    pub host: String,
    /// De dónde salió `host` (DOCKER_HOST, contexto, socket encontrado…).
    pub source: String,
    /// Rotación de logs para contenedores nuevos: (max-size, max-file). None = no tocar.
    pub log_rotation: Option<(String, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Start,
    Stop,
    Restart,
    Kill,
    Pause,
    Unpause,
    Remove,
}

#[derive(Clone, Debug, Default)]
pub struct RawStat {
    pub cpu_total: u64,
    pub sys_cpu: u64,
    pub online: u32,
    pub mem: u64,
    pub limit: u64,
    pub rx: u64,
    pub tx: u64,
}

/// Todo lo necesario para recrear un contenedor editado sin perder nada.
#[derive(Clone, Debug)]
pub struct EditSource {
    pub id: String,
    pub name: String,
    pub was_running: bool,
    pub old_image_id: String,
    pub config: ContainerConfig,
    pub host_config: HostConfig,
    pub networks: HashMap<String, EndpointSettings>,
    /// Volúmenes anónimos (creados por VOLUME en la imagen) que hay que reutilizar al recrear.
    pub implicit_mounts: Vec<(String, String, bool)>,
    #[allow(dead_code)]
    pub image_cmd: Vec<String>,
    pub image_entrypoint: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DeployReport {
    pub containers: Vec<String>,
    pub network: Option<String>,
    pub credentials: Vec<(String, String, String)>,
    pub ports: Vec<String>,
    pub note: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateOutcome {
    UpToDate,
    Updated,
}

impl Engine {
    /// Busca el daemon (ver `find_host`), se conecta y negocia la versión de la API.
    pub async fn connect() -> Result<Engine> {
        let (host, source) = find_host();
        let d = Docker::connect_with_host(&host).with_context(|| format!("no se pudo configurar la conexión a Docker ({host})"))?;
        let e = Engine { d, host, source, log_rotation: Some(("10m".into(), 3)) };
        e.negotiate().await;
        Ok(e)
    }

    /// Cliente que nunca conecta (pruebas de la interfaz).
    #[cfg(test)]
    pub fn offline() -> Engine {
        let d = Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION).expect("cliente http");
        Engine { d, host: "tcp://127.0.0.1:1".into(), source: "prueba".into(), log_rotation: None }
    }

    /// Baja la versión de la API si el daemon es más viejo que bollard (si no, todo falla con «client version too new»).
    /// La versión se comparte entre clones del cliente, así que sirve llamarla en cualquier momento.
    pub async fn negotiate(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.d.clone().negotiate_version()).await;
    }

    /// ¿El daemon corre en esta máquina? (puertos, carpetas y shell del host son los de aquí)
    pub fn is_local(&self) -> bool {
        self.host.starts_with("unix://")
    }

    /// `usuario@host` y puerto si el daemon es remoto por SSH.
    pub fn ssh_target(&self) -> Option<(String, Option<u16>)> {
        let rest = self.host.strip_prefix("ssh://")?;
        let rest = rest.split('/').next().unwrap_or(rest);
        match rest.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => Some((h.to_string(), p.parse().ok())),
            _ => Some((rest.to_string(), None)),
        }
    }

    pub async fn version(&self) -> Result<String> {
        let v = self.d.version().await?;
        Ok(v.version.unwrap_or_default())
    }

}

mod actions;
mod backup;
mod deploy;
mod edit;
mod exec;
mod files;
mod helpers;
mod images;
mod list;
mod space;
#[cfg(test)]
mod tests;

use helpers::*;
pub use helpers::{find_host, missing_binary_hint, timed};
