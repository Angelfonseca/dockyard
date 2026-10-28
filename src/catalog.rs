//! Catálogo de plantillas y "planes" de despliegue (lo que el formulario edita).

use crate::util::{image_repo, parse_cpus, parse_mem, rand_alnum};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const BUILTIN: &str = include_str!("../templates.toml");

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Catalog {
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Template {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub note: String,
    #[serde(default, rename = "service")]
    pub services: Vec<TService>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct TService {
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub restart: String,
    #[serde(default)]
    pub mem: String,
    #[serde(default)]
    pub cmd: Vec<String>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub stdin_open: bool,
    #[serde(default)]
    pub cpus: String,
    /// Comando (sh) que indica si el servicio está listo; también se espera por él al desplegar stacks.
    pub healthcheck: Option<String>,
    pub dump: Option<String>,
    pub dump_ext: Option<String>,
    /// Comando (sh) que lee un respaldo por stdin y lo restaura.
    pub restore: Option<String>,
}

fn default_role() -> String {
    "app".into()
}

impl Catalog {
    /// Plantillas integradas + las del usuario (`~/.config/dockyard/templates.toml`).
    /// Devuelve también un aviso si el archivo del usuario tiene errores.
    pub fn load() -> (Catalog, Option<String>) {
        let mut cat: Catalog = toml::from_str(BUILTIN).expect("templates.toml integrado inválido");
        let path = crate::util::config_dir().join("templates.toml");
        let mut warn = None;
        if let Ok(text) = std::fs::read_to_string(&path) {
            match toml::from_str::<Catalog>(&text) {
                Ok(user) => {
                    for t in user.templates {
                        if let Some(slot) = cat.templates.iter_mut().find(|x| x.id == t.id) {
                            *slot = t;
                        } else {
                            cat.templates.push(t);
                        }
                    }
                }
                Err(e) => warn = Some(format!("{}: {e}", path.display())),
            }
        }
        (cat, warn)
    }

    pub fn get(&self, id: &str) -> Option<&Template> {
        self.templates.iter().find(|t| t.id == id)
    }

    /// Comando de respaldo conocido para una imagen (por repositorio), con su extensión.
    pub fn dump_for_image(&self, image: &str) -> Option<(String, String)> {
        let s = self.service_for_image(image, |s| s.dump.is_some())?;
        Some((s.dump.clone()?, s.dump_ext.clone().unwrap_or_else(|| "sql".into())))
    }

    /// Comando de restauración conocido para una imagen, con la extensión de sus respaldos.
    pub fn restore_for_image(&self, image: &str) -> Option<(String, String)> {
        let s = self.service_for_image(image, |s| s.restore.is_some())?;
        Some((s.restore.clone()?, s.dump_ext.clone().unwrap_or_else(|| "sql".into())))
    }

    fn service_for_image(&self, image: &str, want: impl Fn(&TService) -> bool) -> Option<&TService> {
        let repo = image_repo(image);
        self.templates.iter().flat_map(|t| &t.services).find(|s| want(s) && image_repo(&s.image) == repo)
    }
}

// ─────────────────────────────── Plan ───────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SvcPlan {
    pub role: String,
    pub primary: bool,
    pub image: String,
    pub ports: Vec<String>,
    pub env: Vec<String>,
    pub volumes: Vec<String>,
    pub restart: String,
    pub mem: String,
    pub cmd: Vec<String>,
    pub tty: bool,
    pub stdin_open: bool,
    /// Límite de CPU («1.5» = núcleo y medio). Vacío = sin límite.
    pub cpus: String,
    pub healthcheck: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub name: String,
    pub template_id: String,
    pub template_name: String,
    pub note: String,
    pub services: Vec<SvcPlan>,
    /// Etiquetas extra para todos los contenedores (ej. origen Git).
    pub labels: BTreeMap<String, String>,
}

pub const RESTARTS: [&str; 4] = ["unless-stopped", "always", "on-failure", "no"];

pub fn valid_name(n: &str) -> bool {
    let mut ch = n.chars();
    match ch.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    n.len() <= 63 && ch.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

impl Plan {
    /// Construye un plan desde una plantilla: resuelve marcadores y genera contraseñas.
    pub fn build(t: &Template, name: &str, tz: Option<&str>, default_restart: &str) -> Plan {
        let primary_idx = t.services.iter().position(|s| s.primary).unwrap_or(0);
        // nombres de contenedor por rol, para {{host:rol}}
        let mut hosts: BTreeMap<String, String> = BTreeMap::new();
        for (i, s) in t.services.iter().enumerate() {
            let cname = if i == primary_idx { name.to_string() } else { format!("{name}-{}", s.role) };
            hosts.entry(s.role.clone()).or_insert(cname);
        }
        let mut secrets: BTreeMap<String, String> = BTreeMap::new();
        let mut res = |s: &str| resolve(s, name, &hosts, &mut secrets);

        let mut services = Vec::new();
        for (i, s) in t.services.iter().enumerate() {
            let mut env: Vec<String> = s.env.iter().map(|e| res(e)).collect();
            if let Some(tz) = tz {
                if !env.iter().any(|e| e.starts_with("TZ=")) {
                    env.push(format!("TZ={tz}"));
                }
            }
            services.push(SvcPlan {
                role: s.role.clone(),
                primary: i == primary_idx,
                image: s.image.clone(),
                ports: s.ports.iter().map(|p| res(p)).collect(),
                env,
                volumes: s.volumes.iter().map(|v| res(v)).collect(),
                restart: if s.restart.is_empty() { default_restart.to_string() } else { s.restart.clone() },
                mem: s.mem.clone(),
                cmd: s.cmd.iter().map(|c| res(c)).collect(),
                tty: s.tty,
                stdin_open: s.stdin_open,
                cpus: s.cpus.clone(),
                healthcheck: s.healthcheck.clone().unwrap_or_default(),
            });
        }
        // El servicio primario siempre va al final del arranque: dependencias primero.
        // (Los nombres ya están resueltos; solo cambia el orden de creación/arranque.)
        let mut plan = Plan {
            name: name.to_string(),
            template_id: t.id.clone(),
            template_name: t.name.clone(),
            note: t.note.clone(),
            services,
            labels: BTreeMap::new(),
        };
        plan.start_order_primary_last();
        plan
    }

    fn start_order_primary_last(&mut self) {
        if let Some(i) = self.services.iter().position(|s| s.primary) {
            let p = self.services.remove(i);
            self.services.push(p);
        }
    }

    pub fn container_name(&self, idx: usize) -> String {
        let s = &self.services[idx];
        if s.primary { self.name.clone() } else { format!("{}-{}", self.name, s.role) }
    }

    pub fn network_name(&self) -> Option<String> {
        if self.services.len() > 1 { Some(format!("{}-net", self.name)) } else { None }
    }

    /// Errores que impiden desplegar.
    pub fn errors(&self) -> Vec<String> {
        let mut e = Vec::new();
        if !valid_name(&self.name) {
            e.push(format!("nombre inválido: «{}»", self.name));
        }
        let mut seen_ports: Vec<(u16, String)> = Vec::new();
        for (i, s) in self.services.iter().enumerate() {
            let who = self.container_name(i);
            if s.image.trim().is_empty() {
                e.push(format!("{who}: falta la imagen"));
            }
            for p in &s.ports {
                match PortSpec::parse(p) {
                    Ok(ps) => {
                        if let Some(h) = ps.host {
                            let key = (h, ps.proto.clone());
                            if seen_ports.contains(&key) {
                                e.push(format!("{who}: puerto {h}/{} repetido en el despliegue", ps.proto));
                            }
                            seen_ports.push(key);
                        }
                    }
                    Err(err) => e.push(format!("{who}: puerto «{p}»: {err}")),
                }
            }
            for v in &s.env {
                if !v.contains('=') || v.starts_with('=') {
                    e.push(format!("{who}: variable «{v}» debe ser CLAVE=valor"));
                }
            }
            for v in &s.volumes {
                if !v.contains(':') {
                    e.push(format!("{who}: volumen «{v}» debe ser origen:destino"));
                }
            }
            if let Err(err) = parse_mem(&s.mem) {
                e.push(format!("{who}: {err}"));
            }
            if let Err(err) = parse_cpus(&s.cpus) {
                e.push(format!("{who}: {err}"));
            }
            if !RESTARTS.contains(&s.restart.as_str()) {
                e.push(format!("{who}: política de reinicio inválida «{}»", s.restart));
            }
        }
        e
    }

    /// Avisos que no bloquean (valores vacíos en claves sensibles, etc.).
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        for (i, s) in self.services.iter().enumerate() {
            for v in &s.env {
                if let Some((k, val)) = v.split_once('=') {
                    let ku = k.to_uppercase();
                    let sensitive = ["TOKEN", "PASS", "SECRET", "KEY"].iter().any(|x| ku.contains(x));
                    if sensitive && val.trim().is_empty() {
                        w.push(format!("{}: {k} está vacío", self.container_name(i)));
                    }
                }
            }
        }
        w
    }

    /// Variables que parecen credenciales, para mostrarlas tras desplegar.
    pub fn credentials(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (i, s) in self.services.iter().enumerate() {
            for v in &s.env {
                if let Some((k, val)) = v.split_once('=') {
                    let ku = k.to_uppercase();
                    let secret = ["PASS", "SECRET", "TOKEN"].iter().any(|x| ku.contains(x)) || ku.ends_with("_KEY");
                    if secret && !val.is_empty() {
                        out.push((self.container_name(i), k.to_string(), val.to_string()));
                    }
                }
            }
            // contraseñas pasadas por argumentos (ej. redis --requirepass X)
            for w in s.cmd.windows(2) {
                if w[0].to_lowercase().contains("pass") {
                    out.push((self.container_name(i), w[0].clone(), w[1].clone()));
                }
            }
        }
        out
    }
}

fn resolve(
    s: &str,
    name: &str,
    hosts: &BTreeMap<String, String>,
    secrets: &mut BTreeMap<String, String>,
) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                if key == "name" {
                    out.push_str(name);
                } else if key == "rand" {
                    out.push_str(&rand_alnum(16));
                } else if let Some(k) = key.strip_prefix("secret:") {
                    let v = secrets.entry(k.trim().to_string()).or_insert_with(|| rand_alnum(20)).clone();
                    out.push_str(&v);
                } else if let Some(r) = key.strip_prefix("host:") {
                    match hosts.get(r.trim()) {
                        Some(h) => out.push_str(h),
                        None => out.push_str(name),
                    }
                } else {
                    out.push_str("{{");
                    out.push_str(&after[..end]);
                    out.push_str("}}");
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

// ─────────────────────────────── Puertos ───────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct PortSpec {
    pub ip: Option<String>,
    pub host: Option<u16>,
    pub container: u16,
    pub proto: String,
}

impl PortSpec {
    /// `[ip:]host:contenedor[/proto]` o `contenedor[/proto]` (solo expuesto).
    pub fn parse(s: &str) -> Result<PortSpec> {
        let s = s.trim();
        let (body, proto) = match s.rsplit_once('/') {
            Some((b, p)) if p == "tcp" || p == "udp" || p == "sctp" => (b, p.to_string()),
            Some((_, p)) => bail!("protocolo desconocido «{p}»"),
            None => (s, "tcp".to_string()),
        };
        let parts: Vec<&str> = body.split(':').collect();
        let port = |x: &str| -> Result<u16> {
            let n: u32 = x.trim().parse().with_context(|| format!("«{x}» no es un puerto"))?;
            if n == 0 || n > 65535 {
                bail!("puerto fuera de rango: {n}");
            }
            Ok(n as u16)
        };
        match parts.len() {
            1 => Ok(PortSpec { ip: None, host: None, container: port(parts[0])?, proto }),
            2 => Ok(PortSpec { ip: None, host: Some(port(parts[0])?), container: port(parts[1])?, proto }),
            3 => {
                if parts[0].is_empty() {
                    bail!("IP vacía");
                }
                Ok(PortSpec { ip: Some(parts[0].to_string()), host: Some(port(parts[1])?), container: port(parts[2])?, proto })
            }
            _ => bail!("formato esperado: [ip:]puerto_host:puerto_contenedor[/udp]"),
        }
    }

    pub fn key(&self) -> String {
        format!("{}/{}", self.container, self.proto)
    }

    pub fn to_text(&self) -> String {
        let proto = if self.proto == "tcp" { String::new() } else { format!("/{}", self.proto) };
        match (&self.ip, self.host) {
            (Some(ip), Some(h)) => format!("{ip}:{h}:{}{proto}", self.container),
            (None, Some(h)) => format!("{h}:{}{proto}", self.container),
            _ => format!("{}{proto}", self.container),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_and_builds() {
        let (cat, warn) = Catalog::load();
        assert!(warn.is_none());
        assert!(cat.templates.len() >= 30, "catálogo corto: {}", cat.templates.len());
        let mut ids = std::collections::HashSet::new();
        for t in &cat.templates {
            assert!(ids.insert(t.id.clone()), "id repetido {}", t.id);
            assert!(!t.services.is_empty(), "{} sin servicios", t.id);
            let plan = Plan::build(t, "prueba", Some("America/Mexico_City"), "unless-stopped");
            let errs = plan.errors();
            if t.id == "custom" {
                assert!(errs.iter().any(|e| e.contains("imagen")));
            } else {
                assert!(errs.is_empty(), "{}: {:?}", t.id, errs);
            }
            for s in &plan.services {
                for v in s.env.iter().chain(s.volumes.iter()).chain(s.cmd.iter()).chain(s.ports.iter()) {
                    assert!(!v.contains("{{"), "{}: marcador sin resolver: {v}", t.id);
                }
            }
        }
    }

    #[test]
    fn secrets_are_shared_across_services() {
        let (cat, _) = Catalog::load();
        let plan = Plan::build(cat.get("nextcloud").unwrap(), "nube", None, "unless-stopped");
        assert_eq!(plan.services.len(), 4);
        // el primario queda al final
        assert!(plan.services.last().unwrap().primary);
        assert_eq!(plan.container_name(plan.services.len() - 1), "nube");
        let app = plan.services.iter().find(|s| s.role == "app").unwrap();
        let db = plan.services.iter().find(|s| s.role == "db").unwrap();
        let pw_app = app.env.iter().find(|e| e.starts_with("MYSQL_PASSWORD=")).unwrap();
        let pw_db = db.env.iter().find(|e| e.starts_with("MARIADB_PASSWORD=")).unwrap();
        assert_eq!(pw_app.split_once('=').unwrap().1, pw_db.split_once('=').unwrap().1);
        assert_eq!(pw_app.split_once('=').unwrap().1.len(), 20);
        assert!(app.env.contains(&"MYSQL_HOST=nube-db".to_string()));
        assert!(app.env.contains(&"REDIS_HOST=nube-redis".to_string()));
        assert_eq!(plan.network_name().as_deref(), Some("nube-net"));
        // dos construcciones distintas generan contraseñas distintas
        let plan2 = Plan::build(cat.get("nextcloud").unwrap(), "nube", None, "unless-stopped");
        assert_ne!(plan.credentials(), plan2.credentials());
    }

    #[test]
    fn ports() {
        let p = PortSpec::parse("8080:80").unwrap();
        assert_eq!((p.host, p.container, p.proto.as_str()), (Some(8080), 80, "tcp"));
        let p = PortSpec::parse("100.69.250.9:8080:80").unwrap();
        assert_eq!(p.ip.as_deref(), Some("100.69.250.9"));
        assert_eq!(p.to_text(), "100.69.250.9:8080:80");
        let p = PortSpec::parse("19132:19132/udp").unwrap();
        assert_eq!(p.key(), "19132/udp");
        assert_eq!(p.to_text(), "19132:19132/udp");
        assert_eq!(PortSpec::parse("80").unwrap().host, None);
        assert!(PortSpec::parse("0:80").is_err());
        assert!(PortSpec::parse("99999:80").is_err());
        assert!(PortSpec::parse("a:b").is_err());
        assert!(PortSpec::parse("80/xyz").is_err());
    }

    #[test]
    fn validation() {
        let (cat, _) = Catalog::load();
        let mut plan = Plan::build(cat.get("postgres").unwrap(), "pg", None, "unless-stopped");
        assert!(plan.errors().is_empty());
        plan.services[0].ports.push("5432:5433".into());
        assert!(plan.errors().iter().any(|e| e.contains("repetido")));
        plan.services[0].ports.pop();
        plan.services[0].env.push("SINIGUAL".into());
        assert!(!plan.errors().is_empty());
        plan.services[0].env.pop();
        plan.services[0].mem = "xx".into();
        assert!(!plan.errors().is_empty());
        plan.services[0].mem = "256m".into();
        plan.name = "mal nombre".into();
        assert!(!plan.errors().is_empty());
        assert!(valid_name("a-b_c.d"));
        assert!(!valid_name("-a"));
    }

    #[test]
    fn dump_lookup() {
        let (cat, _) = Catalog::load();
        let (cmd, ext) = cat.dump_for_image("mariadb:11.4").unwrap();
        assert!(cmd.contains("mariadb-dump"));
        assert_eq!(ext, "sql");
        assert!(cat.dump_for_image("docker.io/library/postgres:16").is_some());
        assert!(cat.dump_for_image("redis:7-alpine").is_none());
    }

    #[test]
    fn warnings_for_empty_tokens() {
        let (cat, _) = Catalog::load();
        let plan = Plan::build(cat.get("cloudflared").unwrap(), "cf", None, "unless-stopped");
        assert!(plan.warnings().iter().any(|w| w.contains("TUNNEL_TOKEN")));
    }
}
