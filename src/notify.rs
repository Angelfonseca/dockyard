//! Avisos cuando un contenedor se detiene o deja de pasar su healthcheck. Se envían con `curl` (opcional, como `git`
//! o `docker compose`): si falta, el aviso lo dice en vez de fallar en silencio.
//!
//! Solo se avisa de cambios. El primer escaneo toma nota del estado y no repite avisos viejos al arrancar.

use crate::engine::Engine;
use crate::model::{CRow, Health, St};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

/// Adónde se manda el aviso.
#[derive(Clone, Debug)]
pub enum Sink {
    /// ntfy: el mensaje va como texto plano al tema (p. ej. https://ntfy.sh/mi-tema).
    Ntfy(String),
    /// Webhook genérico: JSON {"text": "..."} (Slack, Mattermost y similares aceptan ese campo).
    Webhook(String),
}

/// Lo que se sabe de un contenedor en la pasada anterior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    pub down: bool,
    pub unhealthy: bool,
}

fn seen(c: &CRow) -> Seen {
    Seen { down: matches!(c.state, St::Exited | St::Dead), unhealthy: matches!(c.health, Some(Health::Unhealthy)) }
}

/// Mensajes por los cambios desde la pasada anterior, y el estado nuevo para la siguiente.
pub fn transitions(prev: &HashMap<String, Seen>, now: &[CRow]) -> (Vec<String>, HashMap<String, Seen>) {
    let mut msgs = Vec::new();
    let mut next = HashMap::new();
    for c in now {
        let s = seen(c);
        if let Some(p) = prev.get(&c.name) {
            if !p.down && s.down {
                msgs.push(format!("«{}» se detuvo (estado: {:?})", c.name, c.state));
            } else if p.down && !s.down {
                msgs.push(format!("«{}» volvió a funcionar", c.name));
            }
            if !p.unhealthy && s.unhealthy && !s.down {
                msgs.push(format!("«{}» dejó de pasar su healthcheck", c.name));
            } else if p.unhealthy && !s.unhealthy && !s.down {
                msgs.push(format!("«{}» vuelve a estar sano", c.name));
            }
        }
        next.insert(c.name.clone(), s);
    }
    (msgs, next)
}

/// Destino del aviso: de los argumentos o, si faltan, de las variables de entorno. Exactamente uno.
pub fn sink_from(ntfy: Option<String>, webhook: Option<String>) -> Result<Sink> {
    let ntfy = ntfy.or_else(|| std::env::var("DOCKYARD_NTFY_URL").ok());
    let webhook = webhook.or_else(|| std::env::var("DOCKYARD_WEBHOOK_URL").ok());
    match (ntfy, webhook) {
        (Some(u), None) => Ok(Sink::Ntfy(u)),
        (None, Some(u)) => Ok(Sink::Webhook(u)),
        (None, None) => bail!("indica --ntfy o --webhook (o DOCKYARD_NTFY_URL / DOCKYARD_WEBHOOK_URL)"),
        (Some(_), Some(_)) => bail!("elige un solo destino: --ntfy o --webhook"),
    }
}

/// Envía un aviso con `curl`. Si no está instalado o el destino responde mal, devuelve el motivo.
pub fn send(sink: &Sink, text: &str) -> Result<()> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-fsS", "--max-time", "15"]);
    match sink {
        Sink::Ntfy(url) => {
            cmd.args(["-H", "Title: dockyard", "-d", text, url]);
        }
        Sink::Webhook(url) => {
            let payload = serde_json::json!({ "text": format!("dockyard: {text}") }).to_string();
            cmd.args(["-H", "Content-Type: application/json", "-d", &payload, url]);
        }
    }
    let out = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("no se pudo ejecutar curl (instálalo para usar los avisos)")?;
    if !out.status.success() {
        bail!("el aviso no llegó: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Vigila los contenedores cada `every` y avisa de cada cambio. No termina salvo por error del propio proceso.
pub async fn watch(engine: &Engine, sink: &Sink, every: Duration) -> Result<()> {
    let mut prev = HashMap::new();
    loop {
        match engine.list_containers().await {
            Ok(list) => {
                let (msgs, next) = transitions(&prev, &list);
                prev = next;
                for m in msgs {
                    match send(sink, &m) {
                        Ok(()) => println!("aviso enviado: {m}"),
                        Err(e) => eprintln!("aviso no enviado ({m}): {e:#}"),
                    }
                }
            }
            Err(e) => eprintln!("no se pudo leer el estado: {e:#}"),
        }
        tokio::time::sleep(every).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: St, health: Option<Health>) -> CRow {
        CRow {
            id: String::new(),
            name: name.into(),
            image: String::new(),
            image_id: String::new(),
            state,
            status: String::new(),
            health,
            ports: String::new(),
            published: vec![],
            stack: None,
            created: 0,
            mounts: vec![],
            networks: vec![],
            tunnel: None,
        }
    }

    #[test]
    fn first_scan_is_silent_and_only_changes_speak() {
        // el primer escaneo toma nota: un contenedor ya caído al arrancar no genera aviso
        let (msgs, prev) = transitions(&HashMap::new(), &[row("pg", St::Exited, None)]);
        assert!(msgs.is_empty());

        let (msgs, prev) = transitions(&prev, &[row("pg", St::Exited, None)]);
        assert!(msgs.is_empty(), "lo que sigue caído no se repite");

        let (msgs, prev) = transitions(&prev, &[row("pg", St::Running, None)]);
        assert_eq!(msgs, vec!["«pg» volvió a funcionar".to_string()]);

        let (msgs, prev) = transitions(&prev, &[row("pg", St::Running, Some(Health::Unhealthy))]);
        assert_eq!(msgs, vec!["«pg» dejó de pasar su healthcheck".to_string()]);

        let (msgs, prev) = transitions(&prev, &[row("pg", St::Dead, Some(Health::Unhealthy))]);
        assert_eq!(msgs.len(), 1, "caerse cuenta una vez, no también como healthcheck");
        assert!(msgs[0].contains("se detuvo"));

        // volvió a funcionar y, además, vuelve a estar sano: dos avisos
        let (msgs, _) = transitions(&prev, &[row("pg", St::Running, Some(Health::Healthy))]);
        assert_eq!(msgs.len(), 2);
    }
}
