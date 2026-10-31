//! Capa de servicios: reglas que comparten la CLI, la TUI y la web. Cada interfaz recoge la entrada, pide confirmación
//! y muestra el resultado; las comprobaciones previas viven aquí para que todas apliquen lo mismo.

use crate::catalog::{Catalog, Plan, PortSpec};
use crate::engine::{DeployReport, Engine, Prog};
use crate::model::CRow;
use crate::util::latest_backup;
use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

/// Puertos del plan que ya usa otro contenedor o un programa del equipo, como «8080/tcp».
/// `local`: solo si el daemon está en esta máquina se puede preguntar al sistema si el puerto está libre.
pub fn busy_ports(existing: &[CRow], plan: &Plan, local: bool) -> Vec<String> {
    let used: HashSet<(u16, String)> = existing.iter().flat_map(|c| c.published.iter().cloned()).collect();
    let mut out = Vec::new();
    for s in &plan.services {
        for p in &s.ports {
            if let Ok(ps) = PortSpec::parse(p) {
                if let Some(h) = ps.host {
                    if used.contains(&(h, ps.proto.clone())) || (local && !can_bind(h, &ps.proto)) {
                        out.push(format!("{h}/{}", ps.proto));
                    }
                }
            }
        }
    }
    out
}

/// Problemas que impiden desplegar `plan` con los contenedores `existing` ya presentes. Vacío = se puede desplegar.
pub fn deploy_problems(plan: &Plan, existing: &[CRow], local: bool) -> Vec<String> {
    let mut errs = plan.errors();
    for i in 0..plan.services.len() {
        let n = plan.container_name(i);
        if existing.iter().any(|c| c.name == n) {
            errs.push(format!("ya existe un contenedor llamado «{n}»"));
        }
    }
    for p in busy_ports(existing, plan, local) {
        errs.push(format!("el puerto {p} ya está en uso"));
    }
    errs
}

/// Qué limpiar: cada campo activa una categoría; `label` limita la limpieza a los recursos con esa etiqueta.
#[derive(Default, Clone, Debug)]
pub struct PruneOpts {
    pub containers: bool,
    pub images: bool,
    pub build: bool,
    pub networks: bool,
    pub volumes: bool,
    pub label: Option<String>,
}

/// Lo que se hizo. `build_skipped`: la caché de construcción no admite etiqueta, así que con filtro no se toca.
#[derive(Default, Debug, PartialEq)]
pub struct PruneReport {
    pub containers: usize,
    pub images: usize,
    pub networks: usize,
    pub volumes: usize,
    pub volumes_failed: usize,
    pub build_skipped: bool,
    pub freed: u64,
}

/// Borra volúmenes uno a uno. Devuelve (borrados, fallidos): uno en uso no detiene a los demás.
pub async fn remove_volumes(engine: &Engine, names: &[String]) -> (usize, usize) {
    let (mut ok, mut fail) = (0, 0);
    for n in names {
        if engine.remove_volume(n).await.is_ok() {
            ok += 1;
        } else {
            fail += 1;
        }
    }
    (ok, fail)
}

/// Limpia lo pedido en `o`, en el orden de siempre: contenedores, imágenes, caché, redes y volúmenes huérfanos.
pub async fn prune(engine: &Engine, o: &PruneOpts) -> Result<PruneReport> {
    let label = o.label.as_deref();
    let mut r = PruneReport::default();
    if o.containers {
        let (k, s) = engine.prune_containers(label).await?;
        r.containers = k;
        r.freed += s;
    }
    if o.images {
        let (k, s) = engine.prune_images(label).await?;
        r.images = k;
        r.freed += s;
    }
    if o.build {
        if label.is_some() {
            r.build_skipped = true;
        } else {
            r.freed += engine.prune_build(None).await?;
        }
    }
    if o.networks {
        r.networks = engine.prune_networks(label).await?;
    }
    if o.volumes {
        let orphans = engine.orphan_volumes(label).await?;
        let (ok, fail) = remove_volumes(engine, &orphans).await;
        r.volumes = ok;
        r.volumes_failed = fail;
    }
    Ok(r)
}

/// Respalda `target`: un contenedor con el volcado de su base de datos, o un volumen con su copia comprimida.
/// El nombre se busca primero entre los contenedores; si no es uno, debe ser un volumen existente.
pub async fn backup(engine: &Engine, catalog: &Catalog, dir: &Path, target: &str, progress: Prog<'_>) -> Result<(PathBuf, u64)> {
    let all = engine.list_containers().await?;
    if let Some(c) = all.iter().find(|c| c.name == target) {
        let (script, ext) = catalog
            .dump_for_image(&c.image)
            .ok_or_else(|| anyhow!("no conozco un comando de respaldo para {}; respalda sus volúmenes", c.image))?;
        if !c.is_up() {
            bail!("«{}» debe estar en ejecución para respaldarlo", c.name);
        }
        return engine.backup_dump(&c.id, &c.name, &script, &ext, dir).await;
    }
    if !engine.volume_exists(target).await? {
        bail!("«{target}» no es un contenedor ni un volumen");
    }
    engine.backup_volume(target, dir, progress).await
}

/// Qué se restaurará y desde qué archivo. Se decide antes de confirmar, para poder mostrarlo al usuario.
#[derive(Debug)]
pub enum RestorePlan {
    /// Volcado SQL cargado dentro de un contenedor en ejecución.
    Sql { id: String, name: String, script: String, file: PathBuf },
    /// Copia comprimida extraída en un volumen.
    Volume { name: String, file: PathBuf },
}

impl RestorePlan {
    pub fn name(&self) -> &str {
        match self {
            RestorePlan::Sql { name, .. } | RestorePlan::Volume { name, .. } => name,
        }
    }
    pub fn file(&self) -> &Path {
        match self {
            RestorePlan::Sql { file, .. } | RestorePlan::Volume { file, .. } => file,
        }
    }
}

/// Decide el destino de una restauración. Si `file` es `None`, usa el respaldo más reciente de ese nombre.
pub async fn plan_restore(engine: &Engine, catalog: &Catalog, dir: &Path, target: &str, file: Option<PathBuf>) -> Result<RestorePlan> {
    let all = engine.list_containers().await?;
    if let Some(c) = all.iter().find(|c| c.name == target) {
        let (script, ext) = catalog
            .restore_for_image(&c.image)
            .ok_or_else(|| anyhow!("no conozco cómo restaurar en {}; si quieres restaurar un volumen, indica su nombre", c.image))?;
        if !c.is_up() {
            bail!("«{}» debe estar en ejecución para restaurar", c.name);
        }
        let file = match file {
            Some(f) => f,
            None => latest_backup(dir, &c.name, &ext).ok_or_else(|| anyhow!("no hay respaldos de «{}» en {}", c.name, dir.display()))?,
        };
        return Ok(RestorePlan::Sql { id: c.id.clone(), name: c.name.clone(), script, file });
    }
    if !engine.volume_exists(target).await? {
        bail!("«{target}» no es un contenedor ni un volumen");
    }
    let file = match file {
        Some(f) => f,
        None => latest_backup(dir, target, "tar.gz").ok_or_else(|| anyhow!("no hay respaldos de «{target}» en {}", dir.display()))?,
    };
    Ok(RestorePlan::Volume { name: target.to_string(), file })
}

/// Ejecuta una restauración ya planificada. `clean` vacía el volumen antes de extraer (solo volúmenes).
pub async fn run_restore(engine: &Engine, plan: &RestorePlan, clean: bool, progress: Prog<'_>) -> Result<()> {
    match plan {
        RestorePlan::Sql { id, script, file, .. } => engine.exec_from_file(id, script, file, progress).await.map(|_| ()),
        RestorePlan::Volume { name, file } => engine.restore_volume(name, file, clean, progress).await,
    }
}

/// Despliega `plan` tras comprobarlo contra el estado actual del daemon.
pub async fn deploy(engine: &Engine, plan: &Plan, progress: Prog<'_>) -> Result<DeployReport> {
    let existing = engine.list_containers().await?;
    let errs = deploy_problems(plan, &existing, engine.is_local());
    if !errs.is_empty() {
        bail!("{}", errs.join("\n"));
    }
    engine.deploy(plan, progress).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::model::St;

    fn row(name: &str, published: Vec<(u16, String)>) -> CRow {
        CRow {
            id: String::new(),
            name: name.into(),
            image: String::new(),
            image_id: String::new(),
            state: St::Running,
            status: String::new(),
            health: None,
            ports: String::new(),
            published,
            stack: None,
            created: 0,
            mounts: vec![],
            networks: vec![],
            tunnel: None,
        }
    }

    fn plan_with_port(port: &str) -> Plan {
        let (cat, _) = Catalog::load();
        let mut plan = Plan::build(cat.get("postgres").unwrap(), "pg", None, "unless-stopped");
        plan.services[0].ports = vec![port.to_string()];
        plan
    }

    #[test]
    fn name_and_port_conflicts_are_reported() {
        let plan = plan_with_port("5433:5432");
        let existing = vec![row("pg", vec![(5433, "tcp".to_string())])];
        let errs = deploy_problems(&plan, &existing, false);
        assert!(errs.iter().any(|e| e.contains("ya existe un contenedor llamado «pg»")), "{errs:?}");
        assert!(errs.iter().any(|e| e == "el puerto 5433/tcp ya está en uso"), "{errs:?}");
    }

    #[test]
    fn free_names_and_ports_pass() {
        let plan = plan_with_port("5433:5432");
        let existing = vec![row("otro", vec![(8080, "tcp".to_string())])];
        // local = false: no se consulta el sistema, así que solo cuentan los contenedores
        assert!(busy_ports(&existing, &plan, false).is_empty());
        assert!(!deploy_problems(&plan, &existing, false).iter().any(|e| e.contains("ya existe") || e.contains("ya está en uso")));
    }

    #[test]
    fn same_port_different_protocol_is_free() {
        let plan = plan_with_port("5433:5432/udp");
        let existing = vec![row("otro", vec![(5433, "tcp".to_string())])];
        assert!(busy_ports(&existing, &plan, false).is_empty());
    }
}
