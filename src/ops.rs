//! Capa de servicios: reglas que comparten la CLI, la TUI y la web. Cada interfaz recoge la entrada, pide confirmación
//! y muestra el resultado; las comprobaciones previas viven aquí para que todas apliquen lo mismo.

use crate::app::can_bind;
use crate::catalog::{Plan, PortSpec};
use crate::engine::{DeployReport, Engine, Prog};
use crate::model::CRow;
use anyhow::{Result, bail};
use std::collections::HashSet;

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
