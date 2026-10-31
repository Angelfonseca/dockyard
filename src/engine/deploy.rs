//! Despliegue de un plan: crear y arrancar servicios en orden, esperando dependencias sanas.

use super::*;

impl Engine {
    // ───────────────────────────── Despliegue ─────────────────────────────

    /// Rotación de logs si el driver por defecto del daemon la admite (json-file, local).
    pub async fn log_config(&self) -> Option<HostConfigLogConfig> {
        let (size, files) = self.log_rotation.clone()?;
        if size.is_empty() {
            return None;
        }
        let drv = self.d.info().await.ok()?.logging_driver?;
        if drv != "json-file" && drv != "local" {
            return None;
        }
        let mut c = HashMap::new();
        c.insert("max-size".to_string(), size);
        c.insert("max-file".to_string(), files.max(1).to_string());
        Some(HostConfigLogConfig { typ: Some(drv), config: Some(c) })
    }

    pub(super) fn svc_body(&self, plan: &Plan, idx: usize, log: &Option<HostConfigLogConfig>) -> Result<ContainerCreateBody> {
        let s = &plan.services[idx];
        let mut exposed: Vec<String> = Vec::new();
        let mut bindings: PortMap = HashMap::new();
        for p in &s.ports {
            let ps = PortSpec::parse(p)?;
            let key = ps.key();
            if !exposed.contains(&key) {
                exposed.push(key.clone());
            }
            if let Some(h) = ps.host {
                let e = bindings.entry(key).or_insert_with(|| Some(Vec::new()));
                if let Some(v) = e.as_mut() {
                    v.push(PortBinding { host_ip: ps.ip.clone(), host_port: Some(h.to_string()) });
                }
            }
        }
        let mut labels = HashMap::new();
        labels.insert("dockyard.managed".to_string(), "true".to_string());
        labels.insert("dockyard.stack".to_string(), plan.name.clone());
        labels.insert("dockyard.template".to_string(), plan.template_id.clone());
        labels.insert("dockyard.role".to_string(), s.role.clone());
        for (k, v) in &plan.labels {
            labels.insert(k.clone(), v.clone());
        }

        let host_config = HostConfig {
            binds: if s.volumes.is_empty() { None } else { Some(s.volumes.clone()) },
            port_bindings: if bindings.is_empty() { None } else { Some(bindings) },
            restart_policy: Some(restart_policy(&s.restart)),
            memory: parse_mem(&s.mem)?,
            nano_cpus: parse_cpus(&s.cpus)?,
            network_mode: plan.network_name(),
            log_config: log.clone(),
            ..Default::default()
        };
        Ok(ContainerCreateBody {
            image: Some(s.image.trim().to_string()),
            env: if s.env.is_empty() { None } else { Some(s.env.clone()) },
            cmd: if s.cmd.is_empty() { None } else { Some(s.cmd.clone()) },
            exposed_ports: if exposed.is_empty() { None } else { Some(exposed) },
            tty: if s.tty { Some(true) } else { None },
            open_stdin: if s.stdin_open { Some(true) } else { None },
            healthcheck: health_config(&s.healthcheck),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        })
    }

    /// Espera a que un servicio recién iniciado esté sano. Error solo si se cae; si tarda o su
    /// healthcheck falla, devuelve un aviso y el despliegue sigue.
    pub(super) async fn wait_ready(&self, name: &str, limit: Duration, progress: Prog<'_>) -> Result<Option<String>> {
        let t0 = std::time::Instant::now();
        loop {
            let i = self.d.inspect_container(name, None::<InspectContainerOptions>).await?;
            let st = i.state.unwrap_or_default();
            let restarts = i.restart_count.unwrap_or(0);
            let status = st.status.map(|s| s.to_string()).unwrap_or_default();
            if status == "exited" || status == "dead" || restarts >= 3 {
                let mut logs = String::new();
                let mut s = self.log_stream(name, "15", false, false);
                while let Some(Ok(l)) = s.next().await {
                    logs.push_str(&l.to_string());
                }
                let clean: Vec<String> = logs.trim().lines().map(strip_ansi).collect();
                bail!("{name} se detuvo al iniciar (código {}). Últimos logs:\n{}", st.exit_code.unwrap_or(-1), clean.join("\n"));
            }
            let health = st.health.and_then(|h| h.status).map(|s| s.to_string()).unwrap_or_default();
            match health.as_str() {
                "healthy" => return Ok(None),
                "" | "none" => return Ok(None),
                "unhealthy" => return Ok(Some(format!("{name}: su healthcheck falla; revisa sus logs"))),
                _ => {}
            }
            if t0.elapsed() > limit {
                return Ok(Some(format!("{name}: no estuvo listo en {} s; se continuó igual", limit.as_secs())));
            }
            progress(format!("Esperando a que {name} esté listo… {} s", t0.elapsed().as_secs()));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Despliega un plan: descarga imágenes, crea red y contenedores, los arranca.
    /// Si algo falla, deshace lo creado.
    pub async fn deploy(&self, plan: &Plan, progress: Prog<'_>) -> Result<DeployReport> {
        let errs = plan.errors();
        if !errs.is_empty() {
            bail!("{}", errs.join("; "));
        }
        let existing = self.list_containers().await?;
        for i in 0..plan.services.len() {
            let n = plan.container_name(i);
            if existing.iter().any(|c| c.name == n) {
                bail!("ya existe un contenedor llamado «{n}»");
            }
        }
        for s in &plan.services {
            self.ensure_image(&s.image, progress).await?;
        }
        let mut created: Vec<String> = Vec::new();
        let mut net_created: Option<String> = None;
        // volúmenes con nombre que este despliegue va a crear (para limpiarlos si se revierte)
        let before: std::collections::HashSet<String> = self
            .d
            .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
            .await
            .ok()
            .and_then(|r| r.volumes)
            .map(|v| v.into_iter().map(|x| x.name).collect())
            .unwrap_or_default();
        let mut new_vols: Vec<String> = Vec::new();
        for s in &plan.services {
            for v in &s.volumes {
                let src = v.split(':').next().unwrap_or("");
                if !src.is_empty() && !src.starts_with(['/', '.', '~']) && !before.contains(src) && !new_vols.iter().any(|x| x == src) {
                    new_vols.push(src.to_string());
                }
            }
        }
        let log = self.log_config().await;
        let mut warnings: Vec<String> = Vec::new();
        let result: Result<()> = async {
            if let Some(net) = plan.network_name() {
                progress(format!("Creando red {net}…"));
                let mut labels = HashMap::new();
                labels.insert("dockyard.stack".to_string(), plan.name.clone());
                self.d
                    .create_network(NetworkCreateRequest {
                        name: net.clone(),
                        driver: Some("bridge".into()),
                        labels: Some(labels),
                        ..Default::default()
                    })
                    .await
                    .with_context(|| format!("no se pudo crear la red {net}"))?;
                net_created = Some(net);
            }
            for i in 0..plan.services.len() {
                let name = plan.container_name(i);
                progress(format!("Creando {name}…"));
                let body = self.svc_body(plan, i, &log)?;
                let opts = CreateContainerOptionsBuilder::default().name(&name).build();
                self.d.create_container(Some(opts), body).await.with_context(|| format!("al crear {name}"))?;
                created.push(name.clone());
                progress(format!("Iniciando {name}…"));
                if let Err(e) = self.d.start_container(&name, None::<StartContainerOptions>).await {
                    let m = e.to_string();
                    let hint = if m.contains("port is already allocated") || m.contains("address already in use") {
                        " — el puerto ya está ocupado"
                    } else if m.contains("executable file not found") {
                        " — la imagen no tiene ese programa"
                    } else {
                        ""
                    };
                    bail!("al iniciar {name}{hint}: {m}");
                }
                // dependencias (todo lo que no es el principal): esperar a que estén listas
                let s = &plan.services[i];
                if !s.primary && !s.healthcheck.is_empty() {
                    if let Some(w) = self.wait_ready(&name, Duration::from_secs(180), progress).await? {
                        warnings.push(w);
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            for n in created.iter().rev() {
                let _ = self
                    .d
                    .remove_container(n, Some(RemoveContainerOptionsBuilder::default().force(true).v(true).build()))
                    .await;
            }
            if let Some(net) = &net_created {
                let _ = self.d.remove_network(net).await;
            }
            for v in &new_vols {
                let _ = self.remove_volume(v).await;
            }
            return Err(e.context("despliegue cancelado y revertido"));
        }
        let mut ports = Vec::new();
        for (i, s) in plan.services.iter().enumerate() {
            for p in &s.ports {
                if let Ok(ps) = PortSpec::parse(p) {
                    if let Some(h) = ps.host {
                        let ip = ps.ip.as_ref().map(|i| format!("{i}:")).unwrap_or_default();
                        ports.push(format!("{} → {ip}{}:{}/{}", plan.container_name(i), h, ps.container, ps.proto));
                    }
                }
            }
        }
        Ok(DeployReport {
            containers: created,
            network: net_created,
            credentials: plan.credentials(),
            ports,
            note: plan.note.clone(),
            warnings,
        })
    }

}
