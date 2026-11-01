//! Comprobación de puertos ocupados en el formulario.

use super::*;
use crate::ops::can_bind;

impl App {
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

}
