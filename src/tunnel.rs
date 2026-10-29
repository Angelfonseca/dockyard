//! Túneles públicos para probar en un sandbox: un contenedor `cloudflared` (sin cuenta) o `ngrok` (con authtoken)
//! que publica UN puerto HTTP de otro contenedor en una URL de internet. Es exposición pública: por eso se pide
//! confirmación en cada interfaz, caducan solos (por defecto 2 h), se cierran si el contenedor destino se recrea
//! y el contenedor del túnel corre sin privilegios.

use crate::engine::Engine;
use crate::model::CRow;
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use bollard::models::*;
use bollard::query_parameters::*;
use futures_util::StreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Etiqueta de los contenedores-túnel; su valor es el nombre del contenedor destino.
pub const LABEL: &str = "dockyard.tunnel";
pub const PREFIX: &str = "dockyard-tunnel-";
const CF_IMAGE: &str = "cloudflare/cloudflared:latest";
const NGROK_IMAGE: &str = "ngrok/ngrok:latest";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    Cloudflare,
    Ngrok,
}

impl Provider {
    pub fn parse(s: &str) -> Result<Provider> {
        match s.trim().to_lowercase().as_str() {
            "cloudflare" | "cloudflared" | "cf" => Ok(Provider::Cloudflare),
            "ngrok" => Ok(Provider::Ngrok),
            o => bail!("proveedor desconocido «{o}» (usa cloudflare o ngrok)"),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Provider::Cloudflare => "cloudflare",
            Provider::Ngrok => "ngrok",
        }
    }
}

pub struct Opts {
    pub provider: Provider,
    pub port: u16,
    /// Horas hasta que se cierra solo; 0 = sin límite.
    pub hours: u32,
    /// Solo ngrok. Si falta se usa DOCKYARD_NGROK_AUTHTOKEN.
    pub authtoken: Option<String>,
    /// Solo ngrok: pide usuario y contraseña al visitante.
    pub basic_auth: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Tunnel {
    pub name: String,
    pub target: String,
    pub port: u16,
    pub provider: String,
    pub url: Option<String>,
    pub expires: i64,
    /// «usuario:contraseña» si se protegió con basic auth.
    pub auth: Option<String>,
    pub running: bool,
}

/// URL pública que anuncia el proveedor en sus logs.
pub fn find_url(p: Provider, logs: &str) -> Option<String> {
    match p {
        // cloudflared: una caja con «https://palabras-al-azar.trycloudflare.com» (api.trycloudflare.com es su API, no el túnel)
        Provider::Cloudflare => logs
            .split(|c: char| c.is_whitespace() || c == '|' || c == '"')
            .find(|t| t.starts_with("https://") && t.ends_with(".trycloudflare.com") && !t.starts_with("https://api."))
            .map(str::to_string),
        // ngrok (--log-format=json): {"msg":"started tunnel",…,"url":"https://….ngrok-free.app"}
        Provider::Ngrok => logs.lines().filter(|l| l.contains("started tunnel")).find_map(|l| {
            let rest = l.split("\"url\":\"").nth(1)?;
            let u = rest.split('"').next()?;
            u.starts_with("https://").then(|| u.to_string())
        }),
    }
}

impl Engine {
    /// Puertos TCP que el contenedor declara (EXPOSE o publicados): solo esos se pueden abrir.
    pub async fn tunnel_ports(&self, id: &str) -> Result<Vec<u16>> {
        let i = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        let mut keys: Vec<String> = i.config.and_then(|c| c.exposed_ports).unwrap_or_default();
        keys.extend(i.network_settings.and_then(|n| n.ports).map(|p| p.into_keys().collect::<Vec<_>>()).unwrap_or_default());
        keys.extend(i.host_config.and_then(|h| h.port_bindings).map(|p| p.into_keys().collect::<Vec<_>>()).unwrap_or_default());
        let mut v: Vec<u16> = keys
            .iter()
            .filter_map(|k| {
                let (p, proto) = k.split_once('/').unwrap_or((k.as_str(), "tcp"));
                if proto == "tcp" { p.parse().ok() } else { None }
            })
            .collect();
        v.sort();
        v.dedup();
        Ok(v)
    }

    async fn tunnel_logs(&self, name: &str) -> String {
        let mut s = Box::pin(self.log_stream(name, "200", false, false));
        let mut out = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(l)) = s.next().await {
                out.push_str(&l.to_string());
            }
        })
        .await;
        out.lines().map(strip_ansi).collect::<Vec<_>>().join("\n")
    }

    /// Abre un túnel público hacia `target:port`. Espera a que el proveedor anuncie la URL.
    pub async fn tunnel_start(&self, target: &str, o: &Opts, progress: crate::engine::Prog<'_>) -> Result<Tunnel> {
        let i = self
            .d
            .inspect_container(target, None::<InspectContainerOptions>)
            .await
            .map_err(|_| anyhow!("no existe el contenedor «{target}»"))?;
        let tname = i.name.clone().unwrap_or_default().trim_start_matches('/').to_string();
        let labels = i.config.as_ref().and_then(|c| c.labels.clone()).unwrap_or_default();
        if tname.starts_with(PREFIX) || labels.contains_key(LABEL) || labels.contains_key("dockyard.helper") {
            bail!("no se abre un túnel hacia un túnel ni hacia un auxiliar de dockyard");
        }
        if !i.state.as_ref().and_then(|s| s.running).unwrap_or(false) {
            bail!("«{tname}» no está en ejecución");
        }
        let ports = self.tunnel_ports(target).await?;
        if !ports.contains(&o.port) {
            let have = ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
            bail!("«{tname}» no declara el puerto {} (tiene: {})", o.port, if have.is_empty() { "ninguno".into() } else { have });
        }
        // Dirección del destino dentro de Docker. En redes propias el nombre sigue al contenedor si se recrea;
        // en la red `bridge` por defecto no hay DNS y se usa la IP (por eso el túnel se cierra si el id cambia).
        let mode = i.host_config.as_ref().and_then(|h| h.network_mode.clone()).unwrap_or_default();
        let (net, addr) = if mode == "host" {
            ("host".to_string(), "127.0.0.1".to_string())
        } else {
            let nets = i.network_settings.as_ref().and_then(|n| n.networks.clone()).unwrap_or_default();
            let mut c: Vec<(String, String)> =
                nets.into_iter().filter_map(|(k, v)| v.ip_address.filter(|x| !x.is_empty()).map(|ip| (k, ip))).collect();
            c.sort_by_key(|(k, _)| (k == "bridge", k.clone()));
            let (net, ip) = c.into_iter().next().ok_or_else(|| anyhow!("«{tname}» no tiene red con IP"))?;
            let host = if net == "bridge" { ip } else { tname.clone() };
            (net, host)
        };

        let name = format!("{PREFIX}{tname}-{}", o.port);
        if self.d.inspect_container(&name, None::<InspectContainerOptions>).await.is_ok() {
            bail!("ya hay un túnel para {tname}:{} ({name}); ciérralo antes de abrir otro", o.port);
        }
        let (image, cmd, env, auth) = match o.provider {
            Provider::Cloudflare => (CF_IMAGE, vec!["tunnel".to_string(), "--url".into(), format!("http://{addr}:{}", o.port)], vec![], None),
            Provider::Ngrok => {
                let token = o
                    .authtoken
                    .clone()
                    .or_else(|| std::env::var("DOCKYARD_NGROK_AUTHTOKEN").ok())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| anyhow!("ngrok necesita un authtoken (dashboard.ngrok.com → Your Authtoken): pásalo o define DOCKYARD_NGROK_AUTHTOKEN"))?;
                if !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                    bail!("el authtoken de ngrok tiene caracteres inválidos");
                }
                let mut cmd = vec!["http".to_string(), format!("{addr}:{}", o.port), "--log=stdout".into(), "--log-format=json".into()];
                let auth = o.basic_auth.then(|| format!("dockyard:{}", rand_alnum(16)));
                if let Some(a) = &auth {
                    cmd.push(format!("--basic-auth={a}"));
                }
                (NGROK_IMAGE, cmd, vec![format!("NGROK_AUTHTOKEN={token}")], auth)
            }
        };
        let secrets: Vec<String> = env.iter().filter_map(|e| e.split_once('=').map(|(_, v)| v.to_string())).chain(auth.clone()).collect();
        let expires = if o.hours > 0 { now_secs() + o.hours as i64 * 3600 } else { 0 };
        self.ensure_image(image, progress).await?;

        let mut l = HashMap::new();
        l.insert(LABEL.to_string(), tname.clone());
        l.insert("dockyard.tunnel.target_id".into(), i.id.clone().unwrap_or_default());
        l.insert("dockyard.tunnel.port".into(), o.port.to_string());
        l.insert("dockyard.tunnel.provider".into(), o.provider.id().into());
        l.insert("dockyard.tunnel.expires".into(), expires.to_string());
        if let Some(a) = &auth {
            l.insert("dockyard.tunnel.auth".into(), a.clone());
        }
        let body = ContainerCreateBody {
            image: Some(image.into()),
            cmd: Some(cmd),
            env: if env.is_empty() { None } else { Some(env) },
            labels: Some(l),
            host_config: Some(HostConfig {
                network_mode: Some(net),
                // la exposición pública no debe volver sola tras un reinicio del servidor
                restart_policy: Some(RestartPolicy { name: Some(RestartPolicyNameEnum::NO), maximum_retry_count: None }),
                memory: Some(128 << 20),
                pids_limit: Some(256),
                cap_drop: Some(vec!["ALL".into()]),
                security_opt: Some(vec!["no-new-privileges:true".into()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.d
            .create_container(Some(CreateContainerOptionsBuilder::default().name(&name).build()), body)
            .await
            .context("no se pudo crear el túnel")?;
        if let Err(e) = self.d.start_container(&name, None::<StartContainerOptions>).await {
            self.remove_helper(&name).await;
            return Err(e.into());
        }

        progress(format!("Abriendo el túnel ({})…", o.provider.id()));
        let t0 = Instant::now();
        let fail = |why: String, logs: String| {
            let mut logs = logs;
            for s in &secrets {
                logs = logs.replace(s.as_str(), "***");
            }
            let tail: Vec<&str> = logs.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
            anyhow!("{why}\n{}", tail.join("\n"))
        };
        loop {
            let logs = self.tunnel_logs(&name).await;
            if let Some(url) = find_url(o.provider, &logs) {
                return Ok(Tunnel { name, target: tname, port: o.port, provider: o.provider.id().into(), url: Some(url), expires, auth, running: true });
            }
            let alive = self
                .d
                .inspect_container(&name, None::<InspectContainerOptions>)
                .await
                .ok()
                .and_then(|x| x.state)
                .and_then(|s| s.running)
                .unwrap_or(false);
            if !alive || t0.elapsed() > Duration::from_secs(45) {
                self.remove_helper(&name).await;
                return Err(fail(if alive { "el proveedor no anunció una URL en 45 s".into() } else { "el túnel se detuvo al iniciar".into() }, logs));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Estado de un túnel: etiquetas + URL actual según sus logs.
    pub async fn tunnel_info(&self, name: &str) -> Result<Tunnel> {
        let i = self.d.inspect_container(name, None::<InspectContainerOptions>).await?;
        let l = i.config.as_ref().and_then(|c| c.labels.clone()).unwrap_or_default();
        let Some(target) = l.get(LABEL).cloned() else { bail!("«{name}» no es un túnel de dockyard") };
        let provider = Provider::parse(l.get("dockyard.tunnel.provider").map(String::as_str).unwrap_or("cloudflare")).unwrap_or(Provider::Cloudflare);
        let running = i.state.as_ref().and_then(|s| s.running).unwrap_or(false);
        let url = if running { find_url(provider, &self.tunnel_logs(name).await) } else { None };
        Ok(Tunnel {
            name: name.to_string(),
            target,
            port: l.get("dockyard.tunnel.port").and_then(|p| p.parse().ok()).unwrap_or(0),
            provider: provider.id().into(),
            url,
            expires: l.get("dockyard.tunnel.expires").and_then(|p| p.parse().ok()).unwrap_or(0),
            auth: l.get("dockyard.tunnel.auth").cloned(),
            running,
        })
    }

    pub async fn tunnels(&self) -> Result<Vec<Tunnel>> {
        let mut out = Vec::new();
        for c in self.list_containers().await?.into_iter().filter(|c| c.tunnel.is_some()) {
            if let Ok(t) = self.tunnel_info(&c.name).await {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Cierra un túnel. Solo borra contenedores que lleven la etiqueta de túnel.
    pub async fn tunnel_stop(&self, name: &str) -> Result<()> {
        let i = self.d.inspect_container(name, None::<InspectContainerOptions>).await.map_err(|_| anyhow!("no existe el túnel «{name}»"))?;
        if !i.config.and_then(|c| c.labels).is_some_and(|l| l.contains_key(LABEL)) {
            bail!("«{name}» no es un túnel de dockyard");
        }
        self.remove_helper(name).await;
        Ok(())
    }

    /// Cierra los túneles vencidos y los que apuntan a un contenedor que ya no existe o que se recreó
    /// (su dirección podría pertenecer ahora a otro contenedor).
    pub async fn sweep_tunnels(&self, rows: &[CRow]) {
        for name in to_close(rows, now_secs()) {
            self.remove_helper(&name).await;
        }
    }
}

/// Nombres de los túneles que deben cerrarse ya.
fn to_close(rows: &[CRow], now: i64) -> Vec<String> {
    rows.iter()
        .filter_map(|t| {
            let r = t.tunnel.as_ref()?;
            let same_target = rows.iter().any(|c| c.name == r.target && (r.target_id.is_empty() || c.id == r.target_id));
            ((r.expires > 0 && now > r.expires) || !same_target).then(|| t.name.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_from_logs() {
        let cf = "2026 INF Requesting new quick Tunnel on trycloudflare.com...\n\
                  ERR Post \"https://api.trycloudflare.com/tunnel\": dial tcp\n\
                  2026 INF +--------------------------------------------+\n\
                  2026 INF |  https://tan-widgets-sing.trycloudflare.com  |\n";
        assert_eq!(find_url(Provider::Cloudflare, cf).as_deref(), Some("https://tan-widgets-sing.trycloudflare.com"));
        assert_eq!(find_url(Provider::Cloudflare, "https://api.trycloudflare.com/tunnel"), None);
        let ng = "{\"msg\":\"starting web service\",\"addr\":\"127.0.0.1:4040\"}\n{\"lvl\":\"info\",\"msg\":\"started tunnel\",\"name\":\"command_line\",\"url\":\"https://ab12.ngrok-free.app\"}";
        assert_eq!(find_url(Provider::Ngrok, ng).as_deref(), Some("https://ab12.ngrok-free.app"));
        assert_eq!(find_url(Provider::Ngrok, "{\"msg\":\"started tunnel\",\"url\":\"tcp://0.tcp.ngrok.io:1\"}"), None);
        assert!(Provider::parse("Cloudflare").is_ok() && Provider::parse("ngrok").is_ok() && Provider::parse("x").is_err());
    }

    fn row(name: &str, id: &str, tunnel: Option<crate::model::TunnelRef>) -> CRow {
        CRow {
            id: id.into(),
            name: name.into(),
            image: "x".into(),
            image_id: String::new(),
            state: crate::model::St::Running,
            status: String::new(),
            health: None,
            ports: String::new(),
            published: vec![],
            stack: None,
            created: 0,
            mounts: vec![],
            networks: vec![],
            tunnel,
        }
    }

    #[test]
    fn expired_or_orphan_tunnels_close() {
        let t = |target: &str, tid: &str, exp: i64| Some(crate::model::TunnelRef { target: target.into(), target_id: tid.into(), expires: exp });
        let rows = vec![
            row("web", "id1", None),
            row("dockyard-tunnel-web-80", "t1", t("web", "id1", 2000)),   // vigente
            row("dockyard-tunnel-old-80", "t2", t("old", "x", 0)),         // su destino ya no existe
            row("dockyard-tunnel-web-81", "t3", t("web", "id-viejo", 0)),  // el destino se recreó (otro id)
            row("dockyard-tunnel-web-82", "t4", t("web", "id1", 900)),     // vencido
            row("dockyard-tunnel-web-83", "t5", t("web", "id1", 0)),       // sin límite y vigente
        ];
        assert_eq!(to_close(&rows, 1000), vec!["dockyard-tunnel-old-80", "dockyard-tunnel-web-81", "dockyard-tunnel-web-82"]);
    }
}
