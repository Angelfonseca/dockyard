//! Funciones auxiliares sin estado: espera acotada, búsqueda del daemon y conversiones.

use super::*;

// ───────────────────────────── Auxiliares ─────────────────────────────

/// Espera como mucho `secs`; si el daemon no contesta, error claro en vez de quedarse colgado.
pub async fn timed<T>(secs: u64, f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .unwrap_or_else(|_| Err(anyhow!("Docker no responde (más de {secs} s)")))
}

/// Dónde está el daemon: DOCKER_HOST → contexto de `docker context` → sockets conocidos
/// (Docker, Docker Desktop, rootless, Colima, OrbStack, Rancher Desktop, Podman). Devuelve (host, origen).
pub fn find_host() -> (String, String) {
    if let Ok(h) = std::env::var("DOCKER_HOST") {
        if !h.trim().is_empty() {
            return (h.trim().to_string(), "DOCKER_HOST".into());
        }
    }
    let cfg_dir = std::env::var_os("DOCKER_CONFIG").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".docker"));
    if let Some((name, h)) = context_host(&cfg_dir, std::env::var("DOCKER_CONTEXT").ok()) {
        return (h, format!("contexto «{name}»"));
    }
    let cands = socket_candidates();
    // primero uno que acepte conexiones; si ninguno, el primero que exista
    for p in &cands {
        if std::os::unix::net::UnixStream::connect(p).is_ok() {
            return (format!("unix://{p}"), "socket encontrado".into());
        }
    }
    for p in &cands {
        if Path::new(p).exists() {
            return (format!("unix://{p}"), "socket encontrado (sin respuesta)".into());
        }
    }
    ("unix:///var/run/docker.sock".into(), "por defecto".into())
}

/// Host del contexto activo de `docker context` (None si es «default» o no hay).
pub(super) fn context_host(cfg_dir: &Path, env_ctx: Option<String>) -> Option<(String, String)> {
    let name = match env_ctx.filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => {
            let txt = std::fs::read_to_string(cfg_dir.join("config.json")).ok()?;
            let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
            v.get("currentContext")?.as_str()?.to_string()
        }
    };
    if name.is_empty() || name == "default" {
        return None;
    }
    // contexts/meta/<sha256(nombre)>/meta.json: se recorre en vez de calcular el hash
    for ent in std::fs::read_dir(cfg_dir.join("contexts").join("meta")).ok()?.flatten() {
        let Ok(txt) = std::fs::read_to_string(ent.path().join("meta.json")) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        if v.get("Name").and_then(|n| n.as_str()) == Some(name.as_str()) {
            let h = v.pointer("/Endpoints/docker/Host")?.as_str()?.to_string();
            return Some((name, h));
        }
    }
    None
}

pub(super) fn socket_candidates() -> Vec<String> {
    let home = home_dir();
    let home = home.display();
    let uid = unsafe { libc::getuid() };
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{uid}"));
    vec![
        "/var/run/docker.sock".into(),
        format!("{home}/.docker/run/docker.sock"),
        format!("{home}/.docker/desktop/docker.sock"),
        format!("{xdg}/docker.sock"),
        format!("{home}/.colima/default/docker.sock"),
        format!("{home}/.orbstack/run/docker.sock"),
        format!("{home}/.rd/docker.sock"),
        format!("{xdg}/podman/podman.sock"),
        "/run/podman/podman.sock".into(),
    ]
}

pub(super) fn id_matches(full: &Option<String>, short: &str) -> bool {
    full.as_deref().map(|f| f.starts_with(short)).unwrap_or(false)
}

pub(super) fn slim_endpoint(ep: &EndpointSettings) -> EndpointSettings {
    EndpointSettings {
        aliases: ep.aliases.clone().filter(|a| !a.is_empty()),
        ipam_config: ep.ipam_config.clone(),
        links: ep.links.clone(),
        ..Default::default()
    }
}

pub(super) fn restart_policy(name: &str) -> RestartPolicy {
    RestartPolicy {
        name: Some(match name {
            "always" => RestartPolicyNameEnum::ALWAYS,
            "unless-stopped" => RestartPolicyNameEnum::UNLESS_STOPPED,
            "on-failure" => RestartPolicyNameEnum::ON_FAILURE,
            _ => RestartPolicyNameEnum::NO,
        }),
        maximum_retry_count: None,
    }
}

pub(super) fn label_filter(label: Option<&str>) -> HashMap<String, Vec<String>> {
    let mut f = HashMap::new();
    if let Some(l) = label {
        f.insert("label".to_string(), vec![l.to_string()]);
    }
    f
}

pub(super) fn health_config(cmd: &str) -> Option<HealthConfig> {
    if cmd.trim().is_empty() {
        return None;
    }
    const S: i64 = 1_000_000_000;
    Some(HealthConfig {
        test: Some(vec!["CMD-SHELL".into(), cmd.to_string()]),
        interval: Some(15 * S),
        timeout: Some(5 * S),
        retries: Some(5),
        start_period: Some(120 * S),
        start_interval: Some(2 * S),
    })
}

pub(super) fn explain_exec_error(e: bollard::errors::Error) -> anyhow::Error {
    let s = e.to_string();
    if s.contains("is not running") || s.contains("is paused") {
        anyhow!("el contenedor no está activo")
    } else {
        anyhow!(s)
    }
}

/// Mensaje claro cuando la imagen no trae el programa pedido (imágenes distroless, scratch…).
pub fn missing_binary_hint(output: &str) -> Option<String> {
    if output.contains("executable file not found") || output.contains("no such file or directory: unknown") {
        Some("La imagen no incluye ese programa (¿imagen distroless o scratch sin shell?). Usa i para inspeccionarla o F para editar archivos.".into())
    } else {
        None
    }
}

pub(super) fn summary_to_row(c: ContainerSummary) -> CRow {
    let name = c
        .names
        .as_ref()
        .and_then(|n| n.first())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_default();
    let state = match c.state {
        Some(ContainerSummaryStateEnum::RUNNING) => St::Running,
        Some(ContainerSummaryStateEnum::PAUSED) => St::Paused,
        Some(ContainerSummaryStateEnum::RESTARTING) => St::Restarting,
        Some(ContainerSummaryStateEnum::CREATED) => St::Created,
        Some(ContainerSummaryStateEnum::EXITED) => St::Exited,
        Some(ContainerSummaryStateEnum::DEAD) => St::Dead,
        _ => St::Other,
    };
    let status = c.status.clone().unwrap_or_default();
    let health: Option<crate::model::Health> = if status.contains("(unhealthy)") {
        Some(crate::model::Health::Unhealthy)
    } else if status.contains("(healthy)") {
        Some(crate::model::Health::Healthy)
    } else if status.contains("health: starting") {
        Some(crate::model::Health::Starting)
    } else {
        None
    };
    let mut ports: Vec<String> = Vec::new();
    let mut published: Vec<(u16, String)> = Vec::new();
    for p in c.ports.clone().unwrap_or_default() {
        let proto = p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into());
        if let Some(h) = p.public_port {
            if !published.contains(&(h, proto.clone())) {
                published.push((h, proto.clone()));
            }
        }
        let sfx = if proto == "tcp" { String::new() } else { format!("/{proto}") };
        let s = match p.public_port {
            Some(h) => {
                let ip = p.ip.clone().filter(|i| i != "0.0.0.0" && i != "::" && !i.is_empty());
                match ip {
                    Some(ip) => format!("{ip}:{h}→{}{sfx}", p.private_port),
                    None => format!("{h}→{}{sfx}", p.private_port),
                }
            }
            None => format!("{}{sfx}", p.private_port),
        };
        if !ports.contains(&s) {
            ports.push(s);
        }
    }
    let labels = c.labels.clone().unwrap_or_default();
    let stack = labels
        .get("dockyard.stack")
        .or_else(|| labels.get("com.docker.compose.project"))
        .cloned();
    let mounts = c
        .mounts
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| match m.typ.as_deref() {
            Some("volume") => m.name,
            _ => m.source,
        })
        .collect();
    let networks = c
        .network_settings
        .and_then(|n| n.networks)
        .map(|n| {
            let mut v: Vec<String> = n.keys().cloned().collect();
            v.sort();
            v
        })
        .unwrap_or_default();
    CRow {
        id: c.id.unwrap_or_default(),
        name,
        image: c.image.unwrap_or_default(),
        image_id: c.image_id.unwrap_or_default(),
        state,
        status,
        health,
        ports: ports.join(", "),
        published,
        stack,
        created: c.created.unwrap_or(0),
        mounts,
        networks,
        tunnel: labels.get(crate::tunnel::LABEL).map(|t| crate::model::TunnelRef {
            target: t.clone(),
            target_id: labels.get("dockyard.tunnel.target_id").cloned().unwrap_or_default(),
            expires: labels.get("dockyard.tunnel.expires").and_then(|e| e.parse().ok()).unwrap_or(0),
        }),
    }
}
