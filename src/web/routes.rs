//! Rutas de la API: cada una valida su entrada y delega en el motor, en la tienda o en los trabajos.

use super::*;

// ───────────────────────────── Rutas ─────────────────────────────

pub(super) fn containers(ctx: &Ctx) -> Vec<CRow> {
    ctx.state.lock().unwrap_or_else(PoisonError::into_inner).snap.as_ref().map(|s| s.containers.clone()).unwrap_or_default()
}

pub(super) fn want_usage(ctx: &Ctx) {
    ctx.state.lock().unwrap_or_else(PoisonError::into_inner).usage_asked = Some(Instant::now());
    if !ctx.shared.want_usage.swap(true, Ordering::Relaxed) {
        ctx.shared.usage_now.notify_one();
    }
}

/// Archivo dentro de la carpeta de respaldos (sin rutas: solo el nombre).
pub(super) fn backup_file(ctx: &Ctx, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        bail!("nombre de archivo inválido");
    }
    let p = ctx.cfg.backups_dir().join(name);
    if !p.is_file() {
        bail!("no existe {}", p.display());
    }
    Ok(p)
}

pub(super) fn host_ports_in_use(ctx: &Ctx) -> HashSet<(u16, String)> {
    containers(ctx).iter().flat_map(|c| c.published.clone()).collect()
}

/// Puertos de host ocupados por otro contenedor o por un programa del equipo.
pub(super) fn busy_ports(ctx: &Ctx, plan: &Plan) -> Vec<String> {
    let used = host_ports_in_use(ctx);
    let local = ctx.engine.is_local();
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

/// Cambia los puertos ocupados por el siguiente libre y les pone la IP elegida.
pub(super) fn fit_ports(ctx: &Ctx, plan: &mut Plan, ip: &str) {
    let mut used = host_ports_in_use(ctx);
    let local = ctx.engine.is_local();
    for s in &mut plan.services {
        for p in &mut s.ports {
            let Ok(mut ps) = PortSpec::parse(p) else { continue };
            if let Some(mut h) = ps.host {
                for _ in 0..200 {
                    let free = !used.contains(&(h, ps.proto.clone())) && (!local || can_bind(h, &ps.proto));
                    if free || h == u16::MAX {
                        break;
                    }
                    h += 1;
                }
                used.insert((h, ps.proto.clone()));
                ps.host = Some(h);
                ps.ip = (!ip.is_empty()).then(|| ip.to_string());
                *p = ps.to_text();
            }
        }
    }
}

/// Origen Git desde el cuerpo JSON de /api/git y /api/git/inspect. La contraseña viaja en el cuerpo (no en la URL de
/// la petición) y no se registra en el trabajo.
pub(super) fn git_source(ctx: &Ctx, body: &[u8]) -> Result<ext::GitSource> {
    let b: Value = serde_json::from_slice(body).context("datos inválidos")?;
    let field = |k: &str| b[k].as_str().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let url = field("url").ok_or_else(|| anyhow!("falta la URL del repositorio"))?;
    let src = ext::GitSource::new(&url, field("ref"), String::new(), field("subdir"), field("user"), field("token"));
    let name = field("name").unwrap_or_else(|| suggest_name(&containers(ctx), &crate::cli::repo_name(&src.url)));
    let mut env = Vec::new();
    for e in b["envs"].as_array().map(Vec::as_slice).unwrap_or_default().iter().take(8) {
        let (name, text) = (e["name"].as_str().unwrap_or(".env").trim().to_string(), e["text"].as_str().unwrap_or_default().to_string());
        // vacío = no tocar el archivo
        if !text.trim().is_empty() {
            let f = ext::EnvFile { name, text };
            f.check()?;
            env.push(f);
        }
    }
    let src = ext::GitSource { name, env, ..src };
    src.validate(true)?;
    Ok(src)
}

pub(super) fn check_plan(ctx: &Ctx, plan: &Plan, new: bool) -> Result<()> {
    let mut errs = plan.errors();
    if new {
        let all = containers(ctx);
        for i in 0..plan.services.len() {
            let n = plan.container_name(i);
            if all.iter().any(|c| c.name == n) {
                errs.push(format!("ya existe un contenedor llamado «{n}»"));
            }
        }
        for p in busy_ports(ctx, plan) {
            errs.push(format!("el puerto {p} ya está en uso"));
        }
    }
    if !errs.is_empty() {
        bail!("{}", errs.join("\n"));
    }
    Ok(())
}

pub(super) async fn route(ctx: &Arc<Ctx>, req: &Req, body: &[u8]) -> Result<Resp> {
    let e = &ctx.engine;
    let get = req.method == "GET";
    let post = req.method == "POST";
    Ok(match req.path.as_str() {
        "/" if get => {
            // un nonce por respuesta: solo corre el script en línea que servimos nosotros
            let nonce = rand_alnum(22);
            let mut r = text("200 OK", &PAGE.replace("<script>", &format!("<script nonce=\"{nonce}\">")));
            r.ctype = "text/html; charset=utf-8";
            r.extra = format!(
                "Content-Security-Policy: default-src 'none'; script-src 'nonce-{nonce}'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
                 font-src https://fonts.gstatic.com; img-src 'self' data: https://cdn.jsdelivr.net; connect-src 'self'; base-uri 'none'; form-action 'none'; \
                 frame-ancestors 'none'; object-src 'none'\r\n\
                 Set-Cookie: dytoken=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict\r\n"
            ); // borra la cookie de versiones anteriores
            r
        }
        "/app.css" if get => {
            let mut r = text("200 OK", CSS);
            r.ctype = "text/css; charset=utf-8";
            r
        }
        "/api/info" if get => {
            let mut access = Vec::new();
            if let Some(ip) = &ctx.bind_ip {
                access.push(json!({ "ip": ip, "label": format!("Solo por la VPN ({ip})"), "hint": "Solo tú lo ves (Tailscale)." }));
            }
            access.push(json!({ "ip": "127.0.0.1", "label": "Solo este equipo", "hint": "Para publicarlo con Caddy u otro proxy." }));
            access.push(json!({ "ip": "", "label": "Toda la red", "hint": "Docker se salta el firewall: queda visible en tu red local." }));
            json_resp(json!({
                "access": access,
                "backups_dir": ctx.cfg.backups_dir().display().to_string(),
                "docker_host": e.host,
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }
        "/api/state" if get => json_resp(state_json(ctx)),
        "/api/job" if get => {
            let id: u64 = req.need("id")?.parse()?;
            let j = ctx.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            json_resp(serde_json::to_value(j.1.get(&id).ok_or_else(|| anyhow!("el trabajo ya no existe"))?)?)
        }

        // ── contenedores ──
        "/api/logs" if get => {
            let id = req.need("id")?;
            let tail = req.q("tail").and_then(|t| t.parse::<u32>().ok()).unwrap_or(300).min(5000);
            let mut s = Box::pin(e.log_stream(id, &tail.to_string(), false, true));
            let mut out = Vec::new();
            let read = async {
                while let Some(chunk) = s.next().await {
                    out.extend_from_slice(chunk?.as_ref());
                }
                Ok::<_, anyhow::Error>(())
            };
            timed(15, read).await?;
            // strip_ansi quita también los saltos de línea: se aplica por línea
            let t = String::from_utf8_lossy(&out).lines().map(strip_ansi).collect::<Vec<_>>().join("\n");
            text("200 OK", &t)
        }
        "/api/container" if get => {
            let id = req.need("id")?;
            let (src, plan) = timed(15, e.load_edit(id)).await?;
            let labels = src.config.labels.clone().unwrap_or_default();
            let origin = if let Some(u) = labels.get("dockyard.git.url") {
                format!("Git: {u}")
            } else if let Some(p) = labels.get("com.docker.compose.project") {
                format!("Compose: proyecto {p}")
            } else if let Some(t) = labels.get("dockyard.template") {
                format!("Tienda: {t}")
            } else {
                "Imagen".to_string()
            };
            let image = plan.services.first().map(|s| s.image.clone()).unwrap_or_default();
            json_resp(json!({
                "name": src.name,
                "plan": plan,
                "credentials": plan.credentials(),
                "networks": src.networks.keys().collect::<Vec<_>>(),
                "origin": origin,
                "dump": ctx.catalog.dump_for_image(&image).is_some(),
                "restore": ctx.catalog.restore_for_image(&image).is_some(),
            }))
        }
        "/api/act" if post => {
            let id = req.need("id")?;
            let act = match req.need("a")? {
                "start" => Act::Start,
                "stop" => Act::Stop,
                "restart" => Act::Restart,
                "remove" => Act::Remove,
                a => bail!("acción desconocida «{a}»"),
            };
            let r = timed(60, e.act(id, act)).await;
            ctx.shared.notify.notify_one();
            r?;
            json_resp(json!({ "ok": true }))
        }
        "/api/down" if post => {
            let stack = req.need("stack")?;
            let n = timed(120, e.down_stack(stack, req.flag("volumes"))).await?;
            ctx.shared.notify.notify_one();
            json_resp(json!({ "removed": n }))
        }
        "/api/exec" if post => {
            let id = req.need("id")?;
            let cmd = String::from_utf8_lossy(body).trim().to_string();
            if cmd.is_empty() {
                bail!("comando vacío");
            }
            let (out, code) = e.exec_collect(id, vec!["sh".into(), "-c".into(), cmd], Duration::from_secs(60)).await?;
            let out = out.lines().map(strip_ansi).collect::<Vec<_>>().join("\n");
            json_resp(json!({ "output": out, "code": code }))
        }
        "/api/update" if post => {
            let id = req.need("id")?.to_string();
            spawn_job(ctx, format!("Actualizando {}", req.q("name").unwrap_or(&id)), |c, j| async move {
                let mut p = progress(&c, j);
                let r = ext::update_container(&c.engine, &id, &mut p).await?;
                Ok(json!({ "updated": matches!(r, UpdateOutcome::Updated) }))
            })
        }
        "/api/edit" if post => {
            let id = req.need("id")?.to_string();
            let plan: Plan = serde_json::from_slice(body).context("datos del formulario inválidos")?;
            check_plan(ctx, &plan, false)?;
            spawn_job(ctx, format!("Aplicando cambios a {}", plan.name), |c, j| async move {
                let mut p = progress(&c, j);
                let (src, _) = c.engine.load_edit(&id).await?;
                c.engine.recreate(&src, &plan, &mut p).await?;
                Ok(json!({ "name": plan.name }))
            })
        }

        // ── tienda ──
        "/api/catalog" if get => {
            let list: Vec<Value> = ctx
                .catalog
                .templates
                .iter()
                .map(|t| {
                    let primary = t.services.iter().find(|s| s.primary).or(t.services.first());
                    json!({
                        "id": t.id,
                        "name": t.name,
                        "category": t.category,
                        "description": t.description,
                        "note": t.note,
                        "images": t.services.iter().map(|s| s.image.clone()).filter(|i| !i.is_empty()).collect::<Vec<_>>(),
                        "image": primary.map(|s| s.image.clone()).unwrap_or_default(),
                        "services": t.services.len(),
                        "ports": primary.map(|s| s.ports.clone()).unwrap_or_default(),
                        "backup": t.services.iter().any(|s| s.dump.is_some()),
                        "svcs": t.services.iter().map(|s| json!({
                            "role": s.role,
                            "primary": s.primary,
                            "image": s.image,
                            "ports": s.ports,
                            "volumes": s.volumes,
                            // solo los nombres; los valores `{{secret:..}}` se generan al instalar
                            "env": s.env.iter().map(|e| {
                                let (k, v) = e.split_once('=').unwrap_or((e, ""));
                                json!({ "k": k, "secret": v.contains("{{secret") || v.contains("{{rand") })
                            }).collect::<Vec<_>>(),
                            "mem": s.mem,
                            "cpus": s.cpus,
                            "health": s.healthcheck.is_some(),
                            "backup": s.dump.is_some(),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json_resp(json!(list))
        }
        "/api/plan" if get => {
            let t = ctx.catalog.get(req.need("t")?).ok_or_else(|| anyhow!("no existe esa plantilla"))?;
            let all = containers(ctx);
            let name = match req.q("name").map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => n.to_string(),
                None => suggest_name(&all, &t.id),
            };
            let mut plan = Plan::build(t, &name, ctx.cfg.tz.as_deref(), &ctx.cfg.restart());
            fit_ports(ctx, &mut plan, req.q("ip").unwrap_or_default());
            json_resp(json!({ "plan": plan, "description": t.description }))
        }
        // Trae un repositorio (público, o privado con usuario y token por https) y lo levanta o prepara el plan.
        "/api/git" if post => {
            let src = git_source(ctx, body)?;
            let title = format!("Git · {}", src.url);
            spawn_job(ctx, title, |c, j| async move {
                let mut p = progress(&c, j);
                let tmpl = c.catalog.get("custom").cloned().unwrap_or_default();
                match ext::git_deploy(&c.engine, &src, &tmpl, c.cfg.tz.as_deref(), &c.cfg.restart(), &mut p).await? {
                    ext::GitOutcome::Compose => Ok(json!({ "name": src.name, "compose": true })),
                    ext::GitOutcome::Image(plan) => Ok(json!({ "name": src.name, "plan": plan })),
                }
            })
        }
        // Mira el repositorio antes de desplegar: qué es y qué archivos de variables le faltan (con su ejemplo).
        "/api/git/inspect" if post => {
            let src = git_source(ctx, body)?;
            spawn_job(ctx, format!("Revisando {}", src.url), |c, j| async move {
                let mut p = progress(&c, j);
                let r = ext::git_inspect(&src, &mut p).await?;
                Ok(json!({
                    "name": src.name,
                    "kind": r.kind,
                    "env_files": r.env_files.iter().map(|(n, ex)| json!({ "name": n, "example": ex })).collect::<Vec<_>>(),
                    "env_example": r.env_example,
                }))
            })
        }

        // ── túneles públicos (sandbox) ──
        "/api/tunnel/info" if get => {
            let id = req.need("id")?;
            let ports = timed(10, e.tunnel_ports(id)).await?;
            let name = containers(ctx).into_iter().find(|c| c.id == id || c.name == id).map(|c| c.name).unwrap_or_default();
            let tunnels: Vec<_> = timed(30, e.tunnels()).await?.into_iter().filter(|t| t.target == name).collect();
            json_resp(json!({ "ports": ports, "tunnels": tunnels, "now": now_secs(), "ngrok_env": std::env::var("DOCKYARD_NGROK_AUTHTOKEN").is_ok_and(|t| !t.is_empty()) }))
        }
        "/api/tunnel" if post => {
            let target = req.need("id")?.to_string();
            let b: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(body).context("datos inválidos")? };
            let o = tunnel::Opts {
                provider: tunnel::Provider::parse(req.need("provider")?)?,
                port: req.need("port")?.parse().context("puerto inválido")?,
                hours: req.q("hours").and_then(|h| h.parse::<u32>().ok()).unwrap_or(2).min(168),
                authtoken: b["authtoken"].as_str().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
                basic_auth: req.flag("auth"),
            };
            spawn_job(ctx, format!("Túnel {} · {target}:{}", o.provider.id(), o.port), |c, j| async move {
                let mut p = progress(&c, j);
                let t = c.engine.tunnel_start(&target, &o, &mut p).await?;
                Ok(json!({ "tunnel": t }))
            })
        }
        "/api/tunnel/stop" if post => {
            e.tunnel_stop(req.need("name")?).await?;
            ctx.shared.notify.notify_one();
            json_resp(json!({ "ok": true }))
        }
        "/api/check" if post => {
            let plan: Plan = serde_json::from_slice(body).context("datos del formulario inválidos")?;
            let mut errors = plan.errors();
            let all = containers(ctx);
            if req.flag("new") {
                for i in 0..plan.services.len() {
                    let n = plan.container_name(i);
                    if all.iter().any(|c| c.name == n) {
                        errors.push(format!("ya existe un contenedor llamado «{n}»"));
                    }
                }
            }
            let busy = if req.flag("new") { busy_ports(ctx, &plan) } else { Vec::new() };
            json_resp(json!({ "errors": errors, "warnings": plan.warnings(), "busy": busy }))
        }
        "/api/deploy" if post => {
            let plan: Plan = serde_json::from_slice(body).context("datos del formulario inválidos")?;
            check_plan(ctx, &plan, true)?;
            spawn_job(ctx, format!("Instalando {}", plan.name), |c, j| async move {
                let mut p = progress(&c, j);
                let r = c.engine.deploy(&plan, &mut p).await?;
                Ok(json!({
                    "name": plan.name,
                    "containers": r.containers,
                    "ports": r.ports,
                    "credentials": r.credentials,
                    "note": r.note,
                    "warnings": r.warnings,
                }))
            })
        }

        // ── imágenes ──
        "/api/images" if get => {
            let list = timed(20, e.list_images(&containers(ctx))).await?;
            let now = now_secs();
            json_resp(json!(list
                .iter()
                .map(|i| json!({
                    "id": i.id,
                    "tags": i.tags,
                    "size": human_bytes(i.size),
                    "bytes": i.size,
                    "age": human_age(now - i.created),
                    "used_by": i.used_by,
                }))
                .collect::<Vec<_>>()))
        }
        "/api/pull" if post => {
            let image = req.need("image")?.trim().to_string();
            spawn_job(ctx, format!("Descargando {image}"), |c, j| async move {
                let mut p = progress(&c, j);
                c.engine.pull(&image, &mut p).await?;
                Ok(json!({ "image": image }))
            })
        }
        "/api/rmi" if post => {
            timed(60, e.remove_image(req.need("id")?)).await?;
            json_resp(json!({ "ok": true }))
        }

        // ── volúmenes ──
        "/api/volumes" if get => {
            want_usage(ctx);
            let list = timed(20, e.list_volumes(&containers(ctx))).await?;
            let st = ctx.state.lock().unwrap_or_else(PoisonError::into_inner);
            let sizes = st.usage.as_ref().map(|u| &u.vol_sizes);
            let ready = sizes.is_some();
            json_resp(json!(list
                .iter()
                .map(|v| {
                    let size = sizes.and_then(|s| s.get(&v.name)).copied();
                    json!({
                        "name": v.name,
                        "driver": v.driver,
                        "used_by": v.used_by,
                        "size": size.map(human_bytes),
                        "bytes": size,
                        "anonymous": v.name.len() == 64 && v.name.bytes().all(|b| b.is_ascii_hexdigit()),
                        "measured": ready,
                    })
                })
                .collect::<Vec<_>>()))
        }
        "/api/rmvol" if post => {
            timed(60, e.remove_volume(req.need("name")?)).await?;
            ctx.shared.usage_now.notify_one();
            json_resp(json!({ "ok": true }))
        }

        // ── respaldos ──
        "/api/backups" if get => {
            let dir = ctx.cfg.backups_dir();
            let mut files: Vec<(i64, Value)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for f in rd.flatten() {
                    let Ok(m) = f.metadata() else { continue };
                    if !m.is_file() {
                        continue;
                    }
                    let name = f.file_name().to_string_lossy().to_string();
                    let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
                    // «nombre-AAAAMMDD-HHMMSS.ext» → nombre
                    let target = name.rsplitn(3, '-').nth(2).unwrap_or("").to_string();
                    let kind = if name.ends_with(".tar.gz") { "volumen" } else { "base de datos" };
                    files.push((mtime, json!({
                        "file": name,
                        "target": target,
                        "kind": kind,
                        "size": human_bytes(m.len()),
                        "age": human_age(now_secs() - mtime),
                    })));
                }
            }
            files.sort_by(|a, b| b.0.cmp(&a.0));
            json_resp(json!({ "dir": dir.display().to_string(), "files": files.into_iter().map(|f| f.1).collect::<Vec<_>>() }))
        }
        "/api/backup" if post => {
            let target = req.need("target")?.to_string();
            spawn_job(ctx, format!("Respaldando {target}"), |c, j| async move {
                let dir = c.cfg.backups_dir();
                let all = c.engine.list_containers().await?;
                let (path, n) = if let Some(ct) = all.iter().find(|x| x.name == target) {
                    let (script, ext) = c
                        .catalog
                        .dump_for_image(&ct.image)
                        .ok_or_else(|| anyhow!("no sé respaldar {}; respalda sus volúmenes", ct.image))?;
                    job_push(&c, j, format!("Volcando la base de datos de {}…", ct.name));
                    c.engine.backup_dump(&ct.id, &ct.name, &script, &ext, &dir).await?
                } else {
                    let mut p = progress(&c, j);
                    c.engine.backup_volume(&target, &dir, &mut p).await?
                };
                job_push(&c, j, format!("✔ {} ({})", path.display(), human_bytes(n)));
                Ok(json!({ "file": path.file_name().map(|f| f.to_string_lossy().to_string()), "size": human_bytes(n) }))
            })
        }
        "/api/restore" if post => {
            let target = req.need("target")?.to_string();
            let file = backup_file(ctx, req.need("file")?)?;
            let clean = req.flag("clean");
            spawn_job(ctx, format!("Restaurando {target}"), |c, j| async move {
                let mut p = progress(&c, j);
                if file.to_string_lossy().ends_with(".tar.gz") {
                    c.engine.restore_volume(&target, &file, clean, &mut p).await?;
                } else {
                    let all = c.engine.list_containers().await?;
                    let ct = all.iter().find(|x| x.name == target).ok_or_else(|| anyhow!("no hay contenedor «{target}»"))?;
                    let (script, _) =
                        c.catalog.restore_for_image(&ct.image).ok_or_else(|| anyhow!("no sé restaurar en {}", ct.image))?;
                    if !ct.is_up() {
                        bail!("«{target}» debe estar en ejecución para restaurar");
                    }
                    c.engine.exec_from_file(&ct.id, &script, &file, &mut p).await?;
                }
                Ok(json!({ "ok": true }))
            })
        }
        // la descarga es un enlace del navegador (sin cabeceras): se pide un ticket de un solo uso y 60 s
        "/api/ticket" if post => {
            let file = req.need("file")?;
            backup_file(ctx, file)?;
            let t = rand_alnum(32);
            let mut tk = ctx.tickets.lock().unwrap_or_else(PoisonError::into_inner);
            tk.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(TICKET_SECS));
            tk.insert(t.clone(), (file.to_string(), Instant::now()));
            json_resp(json!({ "ticket": t }))
        }
        "/api/download" if get => {
            let hit = ctx.tickets.lock().unwrap_or_else(PoisonError::into_inner).remove(req.q("ticket").unwrap_or_default());
            let Some((name, _)) = hit.filter(|(_, at)| at.elapsed() < Duration::from_secs(TICKET_SECS)) else { bail!("enlace de descarga vencido") };
            let path = backup_file(ctx, &name)?;
            // el nombre ya pasó backup_file; aun así se quitan comillas y caracteres de control
            let safe: String = name.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect();
            Resp {
                status: "200 OK",
                ctype: "application/octet-stream",
                body: Vec::new(),
                extra: format!("Content-Disposition: attachment; filename=\"{safe}\"\r\n"),
                file: Some(path),
            }
        }
        "/api/rmbackup" if post => {
            std::fs::remove_file(backup_file(ctx, req.need("file")?)?)?;
            json_resp(json!({ "ok": true }))
        }

        // ── espacio ──
        "/api/space" if get => {
            want_usage(ctx);
            json_resp(space_json(ctx))
        }
        "/api/prune" if post => {
            let what = req.need("what")?;
            let msg = match what {
                "containers" => {
                    let (n, b) = timed(120, e.prune_containers(None)).await?;
                    format!("{n} contenedores detenidos borrados ({} liberados)", human_bytes(b))
                }
                "images" => {
                    let (n, b) = timed(300, e.prune_images(None)).await?;
                    format!("{n} imágenes borradas ({} liberados)", human_bytes(b))
                }
                "build" => format!("caché de build borrada ({} liberados)", human_bytes(timed(300, e.prune_build(None)).await?)),
                "networks" => format!("{} redes borradas", timed(60, e.prune_networks(None)).await?),
                "volumes" => {
                    let vols = timed(30, e.orphan_volumes(None)).await?;
                    let mut n = 0;
                    for v in &vols {
                        if e.remove_volume(v).await.is_ok() {
                            n += 1;
                        }
                    }
                    format!("{n} volúmenes huérfanos borrados")
                }
                w => bail!("no sé limpiar «{w}»"),
            };
            ctx.shared.notify.notify_one();
            ctx.shared.usage_now.notify_one();
            json_resp(json!({ "message": msg }))
        }
        _ => text("404 Not Found", "no existe"),
    })
}

pub(super) fn state_json(ctx: &Ctx) -> Value {
    let st = ctx.state.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(s) = &st.snap else {
        return json!({ "loading": true, "error": st.error });
    };
    let h = &s.host;
    let now = now_secs();
    let containers: Vec<Value> = s
        .containers
        .iter()
        .map(|c| {
            let stat = s.stats.get(&c.id);
            json!({
                "id": c.id,
                "name": c.name,
                "image": c.image,
                "state": c.state.label(),
                "up": c.is_up(),
                "status": c.status,
                "health": c.health.map(|h| match h {
                    Health::Healthy => "sano",
                    Health::Unhealthy => "con fallas",
                    Health::Starting => "iniciando",
                }),
                "ports": c.ports,
                "published": c.published.iter().map(|(p, proto)| json!([p, proto])).collect::<Vec<_>>(),
                "stack": c.stack,
                "age": human_age(now - c.created),
                "mounts": c.mounts,
                "cpu": stat.map(|x| x.cpu),
                "mem": stat.map(|x| human_bytes(x.mem)),
                "mem_pct": stat.and_then(|x| (x.mem_limit > 0).then(|| x.mem as f64 * 100.0 / x.mem_limit as f64)),
                "dump": ctx.catalog.dump_for_image(&c.image).is_some(),
                "tunnel_of": c.tunnel.as_ref().map(|t| t.target.clone()),
            })
        })
        .collect();
    json!({
        "error": st.error,
        "docker": s.docker_version,
        "host": {
            "name": h.hostname,
            "os": h.os,
            "cpu": h.cpu,
            "cores": h.cores.len(),
            "cpu_model": h.cpu_model,
            "temp": h.temp_c,
            "mem_used": human_bytes(h.mem_used),
            "mem_total": human_bytes(h.mem_total),
            "mem_pct": if h.mem_total > 0 { h.mem_used as f64 * 100.0 / h.mem_total as f64 } else { 0.0 },
            "load": [h.load.0, h.load.1, h.load.2],
            "uptime": human_uptime(h.uptime),
            "disks": h.disks.iter().map(|d| json!({
                "mount": d.mount,
                "used": human_bytes(d.used),
                "total": human_bytes(d.total),
                "pct": if d.total > 0 { d.used as f64 * 100.0 / d.total as f64 } else { 0.0 },
            })).collect::<Vec<_>>(),
            "rx": human_rate(h.net_rx_rate),
            "tx": human_rate(h.net_tx_rate),
        },
        "containers": containers,
    })
}

pub(super) fn space_json(ctx: &Ctx) -> Value {
    let st = ctx.state.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(u) = &st.usage else {
        return json!({ "loading": true });
    };
    let row = |id: &str, name: &str, x: &Usage| {
        json!({ "id": id, "name": name, "count": x.count, "active": x.active, "size": human_bytes(x.size), "reclaimable": human_bytes(x.reclaimable), "reclaimable_bytes": x.reclaimable, "known": x.known })
    };
    let mut vols: Vec<(&String, &u64)> = u.vol_sizes.iter().filter(|(_, s)| **s > 0).collect();
    vols.sort_by(|a, b| b.1.cmp(a.1));
    json!({
        "rows": [
            row("images", "Imágenes", &u.images),
            row("containers", "Contenedores", &u.containers),
            row("volumes", "Volúmenes", &u.volumes),
            row("build", "Caché de build", &u.build),
        ],
        "volumes": vols.iter().take(15).map(|(n, s)| json!({ "name": n, "size": human_bytes(**s) })).collect::<Vec<_>>(),
        "root": u.root.as_ref().map(|(p, used, total)| json!({ "path": p, "used": human_bytes(*used), "total": human_bytes(*total), "pct": if *total > 0 { *used as f64 * 100.0 / *total as f64 } else { 0.0 } })),
    })
}

