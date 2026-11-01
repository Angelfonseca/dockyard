//! Edición de un contenedor: lectura de su configuración, recreación, actualización y bajada de stacks.

use super::*;

impl Engine {
    // ───────────────────────────── Edición / recreación ─────────────────────────────

    /// Carga un contenedor existente como `Plan` editable (un solo servicio) + datos para recrearlo.
    pub async fn load_edit(&self, id: &str) -> Result<(EditSource, Plan)> {
        let i = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        let config = i.config.clone().ok_or_else(|| anyhow!("sin configuración"))?;
        let host_config = i.host_config.clone().unwrap_or_default();
        let name = i.name.clone().unwrap_or_default().trim_start_matches('/').to_string();
        let old_image_id = i.image.clone().unwrap_or_default();

        // Lo que la imagen ya define (para no mostrar PATH, etc. como si fuera del usuario)
        let (img_env, img_cmd, img_ep) = match self.d.inspect_image(&old_image_id).await {
            Ok(img) => {
                let c = img.config.unwrap_or_default();
                (c.env.unwrap_or_default(), c.cmd.unwrap_or_default(), c.entrypoint.unwrap_or_default())
            }
            Err(_) => (vec![], vec![], vec![]),
        };

        let env: Vec<String> =
            config.env.clone().unwrap_or_default().into_iter().filter(|e| !img_env.contains(e)).collect();
        let cmd_now = config.cmd.clone().unwrap_or_default();
        let cmd = if cmd_now == img_cmd { vec![] } else { cmd_now };

        // Puertos desde PortBindings
        let mut ports = Vec::new();
        if let Some(pm) = &host_config.port_bindings {
            let mut keys: Vec<&String> = pm.keys().collect();
            keys.sort();
            for k in keys {
                let (cport, proto) = k.split_once('/').unwrap_or((k.as_str(), "tcp"));
                if let Some(Some(list)) = pm.get(k) {
                    for b in list {
                        let hp = b.host_port.clone().unwrap_or_default();
                        if hp.is_empty() {
                            continue;
                        }
                        let ip = b.host_ip.clone().filter(|x| !x.is_empty() && x != "0.0.0.0" && x != "::");
                        let ps = PortSpec {
                            ip,
                            host: hp.parse().ok(),
                            container: cport.parse().unwrap_or(0),
                            proto: proto.to_string(),
                        };
                        ports.push(ps.to_text());
                    }
                }
            }
        }

        let binds = host_config.binds.clone().unwrap_or_default();
        // Mounts que existen pero no están declarados ni en Binds ni en HostConfig.Mounts → implícitos
        let declared: Vec<String> = host_config
            .mounts
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| m.target)
            .chain(binds.iter().filter_map(|b| b.split(':').nth(1).map(|s| s.to_string())))
            .collect();
        let mut implicit = Vec::new();
        for m in i.mounts.clone().unwrap_or_default() {
            let is_vol = m.typ.as_deref() == Some("volume");
            let dest = m.destination.clone().unwrap_or_default();
            if is_vol && !dest.is_empty() && !declared.contains(&dest) {
                if let Some(n) = m.name {
                    implicit.push((n, dest, m.rw.unwrap_or(true)));
                }
            }
        }

        let restart = host_config
            .restart_policy
            .as_ref()
            .and_then(|r| r.name)
            .map(|n| match n {
                RestartPolicyNameEnum::ALWAYS => "always",
                RestartPolicyNameEnum::UNLESS_STOPPED => "unless-stopped",
                RestartPolicyNameEnum::ON_FAILURE => "on-failure",
                _ => "no",
            })
            .unwrap_or("no")
            .to_string();

        let mut networks = i.network_settings.clone().and_then(|n| n.networks).unwrap_or_default();
        for ep in networks.values_mut() {
            if let Some(al) = ep.aliases.as_mut() {
                al.retain(|a| a.len() != 12 || !id_matches(&i.id, a));
            }
        }

        let svc = SvcPlan {
            role: "app".into(),
            primary: true,
            image: config.image.clone().unwrap_or_default(),
            ports,
            env,
            volumes: binds,
            restart,
            mem: mem_to_text(host_config.memory.unwrap_or(0)),
            cmd,
            tty: config.tty.unwrap_or(false),
            stdin_open: config.open_stdin.unwrap_or(false),
            cpus: cpus_to_text(host_config.nano_cpus.unwrap_or(0)),
            healthcheck: String::new(), // se conserva el de la configuración original
        };
        let plan = Plan {
            name: name.clone(),
            template_id: String::new(),
            template_name: format!("Editar «{name}»"),
            note: String::new(),
            services: vec![svc],
            labels: Default::default(),
        };
        let src = EditSource {
            id: i.id.clone().unwrap_or_else(|| id.to_string()),
            name,
            was_running: i.state.as_ref().and_then(|s| s.running).unwrap_or(false),
            old_image_id,
            config,
            host_config,
            networks,
            implicit_mounts: implicit,
            image_cmd: img_cmd,
            image_entrypoint: img_ep,
        };
        Ok((src, plan))
    }

    pub(super) fn recreate_body(&self, src: &EditSource, plan: &Plan, log: Option<HostConfigLogConfig>) -> Result<ContainerCreateBody> {
        let s = &plan.services[0];
        let mut body: ContainerCreateBody = serde_json::from_value(serde_json::to_value(&src.config)?)
            .context("no se pudo reutilizar la configuración original")?;
        body.image = Some(s.image.trim().to_string());
        // Docker combina las variables de la imagen con estas; solo enviamos las del usuario
        body.env = if s.env.is_empty() { None } else { Some(s.env.clone()) };
        body.cmd = if s.cmd.is_empty() { None } else { Some(s.cmd.clone()) };
        if body.entrypoint.as_ref() == Some(&src.image_entrypoint) {
            body.entrypoint = None; // que mande la imagen (por si cambió el tag)
        }
        if let Some(h) = &body.hostname {
            if h.len() == 12 && id_matches(&Some(src.id.clone()), h) {
                body.hostname = None;
            }
        }
        body.tty = Some(s.tty);
        body.open_stdin = Some(s.stdin_open);

        let mut exposed = body.exposed_ports.take().unwrap_or_default();
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
        body.exposed_ports = if exposed.is_empty() { None } else { Some(exposed) };

        let mut hc = src.host_config.clone();
        hc.port_bindings = if bindings.is_empty() { None } else { Some(bindings) };
        let mut binds = s.volumes.clone();
        for (vol, dest, rw) in &src.implicit_mounts {
            binds.push(format!("{vol}:{dest}{}", if *rw { "" } else { ":ro" }));
        }
        hc.binds = if binds.is_empty() { None } else { Some(binds) };
        hc.restart_policy = Some(restart_policy(&s.restart));
        hc.memory = parse_mem(&s.mem)?;
        hc.nano_cpus = parse_cpus(&s.cpus)?;
        // rotación de logs solo si el contenedor no tenía opciones propias y usa el driver por defecto
        if let Some(l) = log {
            let cur = hc.log_config.clone().unwrap_or_default();
            let same_driver = cur.typ.as_deref().map(|t| t.is_empty() || Some(t) == l.typ.as_deref()).unwrap_or(true);
            if same_driver && cur.config.map(|c| c.is_empty()).unwrap_or(true) {
                hc.log_config = Some(l);
            }
        }
        // la red se reconecta aparte
        let primary_net = hc.network_mode.clone();
        let mut endpoints = HashMap::new();
        if let Some(pn) = &primary_net {
            if let Some(ep) = src.networks.get(pn) {
                endpoints.insert(pn.clone(), slim_endpoint(ep));
            }
        }
        if !endpoints.is_empty() {
            body.networking_config = Some(NetworkingConfig { endpoints_config: Some(endpoints) });
        }
        body.host_config = Some(hc);
        Ok(body)
    }

    /// Reemplaza un contenedor por una versión editada. Con reversión si falla.
    pub async fn recreate(&self, src: &EditSource, plan: &Plan, progress: Prog<'_>) -> Result<String> {
        let errs = plan.errors();
        if !errs.is_empty() {
            bail!("{}", errs.join("; "));
        }
        let s = &plan.services[0];
        self.ensure_image(&s.image, progress).await?;
        let log = self.log_config().await;
        let body = self.recreate_body(src, plan, log)?;

        let old_tmp = format!("{}-old-{}", src.name, &utc_stamp()[9..]);
        if src.was_running {
            progress(format!("Deteniendo {}…", src.name));
            self.d
                .stop_container(&src.id, Some(StopContainerOptionsBuilder::default().t(15).build()))
                .await
                .context("no se pudo detener el contenedor original")?;
        }
        self.d
            .rename_container(&src.id, RenameContainerOptionsBuilder::default().name(&old_tmp).build())
            .await
            .context("no se pudo renombrar el original")?;

        let rollback = |why: anyhow::Error| async move {
            let _ = self
                .d
                .rename_container(&src.id, RenameContainerOptionsBuilder::default().name(&src.name).build())
                .await;
            if src.was_running {
                let _ = self.d.start_container(&src.id, None::<StartContainerOptions>).await;
            }
            why.context("cambios revertidos: el contenedor original sigue como estaba")
        };

        progress(format!("Creando {}…", src.name));
        let opts = CreateContainerOptionsBuilder::default().name(&src.name).build();
        let created = match self.d.create_container(Some(opts), body).await {
            Ok(c) => c,
            Err(e) => return Err(rollback(anyhow!("{e}")).await),
        };
        // redes adicionales
        let primary = src.host_config.network_mode.clone();
        for (net, ep) in &src.networks {
            if Some(net.as_str()) == primary.as_deref() || net == "none" || net == "host" {
                continue;
            }
            let _ = self
                .d
                .connect_network(
                    net,
                    NetworkConnectRequest { container: created.id.clone(), endpoint_config: Some(slim_endpoint(ep)) },
                )
                .await;
        }
        if src.was_running {
            progress(format!("Iniciando {}…", src.name));
            if let Err(e) = self.d.start_container(&created.id, None::<StartContainerOptions>).await {
                let _ = self
                    .d
                    .remove_container(&created.id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
                    .await;
                return Err(rollback(anyhow!("{e}")).await);
            }
            // el nuevo tiene que quedar sano: si no, se quita y se vuelve al original
            progress(format!("Comprobando {}…", src.name));
            if !self.settles(&created.id, Duration::from_secs(60)).await {
                let _ = self
                    .d
                    .remove_container(&created.id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
                    .await;
                return Err(rollback(anyhow!("el contenedor nuevo no quedó sano: su healthcheck falla o se detuvo")).await);
            }
        }
        // conservar volúmenes: v=false
        let _ = self
            .d
            .remove_container(&src.id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
            .await;
        Ok(created.id)
    }

    /// ¿El contenedor queda en marcha y sano dentro de `limit`? Con healthcheck: `healthy`. Sin él: en marcha unos
    /// segundos sin reiniciarse. `false` si se detiene, se reinicia, su healthcheck falla o no decide a tiempo.
    pub(super) async fn settles(&self, id: &str, limit: Duration) -> bool {
        let t0 = std::time::Instant::now();
        let mut restarts0: Option<i64> = None;
        loop {
            let Ok(i) = self.d.inspect_container(id, None::<InspectContainerOptions>).await else { return false };
            let st = i.state.unwrap_or_default();
            let status = st.status.map(|s| s.to_string()).unwrap_or_default();
            let restarts = i.restart_count.unwrap_or(0);
            if status == "exited" || status == "dead" || restarts > restarts0.unwrap_or(restarts) {
                return false;
            }
            restarts0.get_or_insert(restarts);
            let health = st.health.and_then(|h| h.status).map(|s| s.to_string()).unwrap_or_default();
            match health.as_str() {
                "healthy" => return true,
                "unhealthy" => return false,
                "" | "none" if status == "running" && t0.elapsed() >= Duration::from_secs(5) => return true,
                _ => {}
            }
            if t0.elapsed() > limit {
                return false;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Descarga la imagen actual de un contenedor y lo recrea si cambió.
    pub async fn update(&self, id: &str, progress: Prog<'_>) -> Result<UpdateOutcome> {
        let (src, plan) = self.load_edit(id).await?;
        let image = plan.services[0].image.clone();
        self.pull(&image, progress).await.with_context(|| format!("no se pudo descargar {image} (¿imagen local?)"))?;
        let new_id = self.image_id(&image).await?;
        if new_id == src.old_image_id {
            return Ok(UpdateOutcome::UpToDate);
        }
        self.recreate(&src, &plan, progress).await?;
        Ok(UpdateOutcome::Updated)
    }

    /// Elimina todos los contenedores de un stack (etiqueta dockyard.stack) y su red.
    pub async fn down_stack(&self, stack: &str, volumes: bool) -> Result<usize> {
        let all = self.list_containers().await?;
        let members: Vec<&CRow> = all.iter().filter(|c| c.stack.as_deref() == Some(stack)).collect();
        if members.is_empty() {
            bail!("no hay contenedores en el stack «{stack}»");
        }
        let mut vols: Vec<String> = Vec::new();
        for c in &members {
            let i = self.d.inspect_container(&c.id, None::<InspectContainerOptions>).await?;
            for m in i.mounts.unwrap_or_default() {
                if m.typ.as_deref() == Some("volume") {
                    if let Some(n) = m.name {
                        if !vols.contains(&n) {
                            vols.push(n);
                        }
                    }
                }
            }
        }
        for c in &members {
            self.act(&c.id, Act::Remove).await?;
        }
        // redes del stack: la de dockyard («<stack>-net») y las de Compose («<proyecto>_default», etc.)
        for n in self.d.list_networks(None::<ListNetworksOptions>).await.unwrap_or_default() {
            let labels = n.labels.unwrap_or_default();
            let name = n.name.unwrap_or_default();
            let mine = labels.get("dockyard.stack").map(|s| s == stack).unwrap_or(false)
                || labels.get("com.docker.compose.project").map(|s| s == stack).unwrap_or(false)
                || name == format!("{stack}-net");
            if mine {
                let _ = self.d.remove_network(&name).await;
            }
        }
        if volumes {
            for v in vols {
                let _ = self.remove_volume(&v).await;
            }
        }
        Ok(members.len())
    }

}
