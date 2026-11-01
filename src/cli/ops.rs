//! Operaciones de la CLI que combinan motor, plan y configuración: despliegue, edición, parada de stacks y ajustes.

use super::*;

pub fn repo_name(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("app");
    let n = last.trim_end_matches(".git");
    let n: String = n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    if n.is_empty() { "app".into() } else { n }
}

pub(super) fn compose_target(path: &Path, name: Option<String>) -> Result<(String, PathBuf)> {
    let file = ext::find_compose_file(path).ok_or_else(|| anyhow!("no encontré compose.yaml ni docker-compose.yml en {}", path.display()))?;
    let file = std::fs::canonicalize(&file)?;
    let project = match name {
        Some(n) => ext::project_name(&n),
        None => ext::project_name(&file.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().to_string()).unwrap_or_default()),
    };
    Ok((project, file))
}

pub(super) async fn down(engine: &Engine, stack: &str, volumes: bool, yes: bool) -> Result<()> {
    let msg = if volumes {
        format!("Se eliminarán TODOS los contenedores del stack «{stack}» y sus volúmenes (datos incluidos). ¿Continuar?")
    } else {
        format!("Se eliminarán los contenedores del stack «{stack}» y sus redes (los volúmenes se conservan). ¿Continuar?")
    };
    if !confirm(&msg, yes)? {
        bail!("cancelado");
    }
    let n = engine.down_stack(stack, volumes).await?;
    println!("✔ {n} contenedores eliminados");
    Ok(())
}

pub(super) async fn deploy_plan(engine: &Engine, plan: Plan, dry_run: bool, yes: bool) -> Result<()> {
    print_plan(&plan);
    let errs = plan.errors();
    if !errs.is_empty() {
        bail!("{}", errs.join("\n"));
    }
    for w in plan.warnings() {
        eprintln!("aviso: {w}");
    }
    if dry_run {
        return Ok(());
    }
    if !confirm("¿Desplegar?", yes)? {
        bail!("cancelado");
    }
    let mut prog = progress_printer();
    let rep = crate::ops::deploy(engine, &plan, &mut prog).await;
    eprintln!();
    let rep = rep?;
    println!("✔ «{}» desplegado: {}", plan.name, rep.containers.join(", "));
    for p in &rep.ports {
        println!("  puerto {p}");
    }
    for w in &rep.warnings {
        println!("  aviso: {w}");
    }
    if !rep.credentials.is_empty() {
        println!("\nCredenciales generadas (también con: dockyard creds {}):", plan.name);
        for (c, k, v) in &rep.credentials {
            println!("  {c:<22} {k} = {v}");
        }
    }
    if !rep.note.is_empty() {
        println!("\nNota: {}", rep.note);
    }
    Ok(())
}

/// Baja el archivo, lo abre en el editor y lo sube si cambió. Devuelve si se guardó.
pub(super) async fn edit_file(engine: &Engine, container: &str, path: &str) -> Result<bool> {
    let (content, header, is_new) = engine.read_file(container, path).await?;
    if is_new {
        eprintln!("(archivo nuevo: {path})");
    }
    let fname = Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "archivo".into());
    let edited = tokio::task::spawn_blocking(move || ext::edit_in_editor(&content, &fname)).await??;
    match edited {
        Some(new) => {
            engine.write_file(container, path, &new, &header).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub(super) fn report_edit(saved: bool, path: &str) {
    if saved {
        println!("✔ {path} guardado");
    } else {
        println!("Sin cambios en {path}");
    }
}

pub(super) async fn each(engine: &Engine, names: &[String], act: Act, done: &str) -> Result<()> {
    if names.is_empty() {
        bail!("indica al menos un contenedor");
    }
    let mut failed = 0;
    for n in names {
        match find(engine, n).await {
            Ok(c) => match engine.act(&c.id, act).await {
                Ok(()) => println!("✔ {} {done}", c.name),
                Err(e) => {
                    failed += 1;
                    eprintln!("✘ {n}: {e}");
                }
            },
            Err(e) => {
                failed += 1;
                eprintln!("✘ {e}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} con error");
    }
    Ok(())
}

pub(super) async fn sample_all(engine: &Engine, list: &[CRow]) -> std::collections::HashMap<String, crate::engine::RawStat> {
    let mut m = std::collections::HashMap::new();
    for c in list {
        if let Ok(s) = engine.raw_stats(&c.id).await {
            m.insert(c.id.clone(), s);
        }
    }
    m
}

pub fn print_plan(plan: &Plan) {
    for (i, s) in plan.services.iter().enumerate() {
        println!("● {}  ({})", plan.container_name(i), s.role);
        println!("    imagen   {}", if s.image.is_empty() { "(sin definir: usa --image)".to_string() } else { s.image.clone() });
        if !s.ports.is_empty() {
            println!("    puertos  {}", s.ports.join("  "));
        }
        for e in &s.env {
            println!("    env      {e}");
        }
        for v in &s.volumes {
            println!("    volumen  {v}");
        }
        if !s.cmd.is_empty() {
            println!("    comando  {}", join_words(&s.cmd));
        }
        let mut lim = String::new();
        if !s.mem.is_empty() {
            lim.push_str(&format!(" · memoria máx. {}", s.mem));
        }
        if !s.cpus.is_empty() {
            lim.push_str(&format!(" · CPU máx. {}", s.cpus));
        }
        println!("    reinicio {}{lim}", s.restart);
    }
    if let Some(n) = plan.network_name() {
        println!("● red interna {n}");
    }
    println!();
}

pub fn apply_overrides(plan: &mut Plan, env: &[String], ports: &[String], tag: Option<&str>, image: Option<&str>) -> Result<()> {
    let roles: Vec<String> = plan.services.iter().map(|s| s.role.clone()).collect();
    let primary = plan.services.iter().position(|s| s.primary).unwrap_or(0);
    for spec in env {
        let Some((k, v)) = spec.split_once('=') else { bail!("«{spec}» debe ser CLAVE=valor") };
        let (target, key): (Option<usize>, &str) = match k.split_once('.') {
            Some((r, key)) if roles.iter().any(|x| x == r) => (roles.iter().position(|x| x == r), key),
            _ => (None, k),
        };
        let idx = match target {
            Some(i) => i,
            None => plan
                .services
                .iter()
                .position(|s| s.env.iter().any(|e| e.split('=').next() == Some(key)))
                .unwrap_or(primary),
        };
        let line = format!("{key}={v}");
        let s = &mut plan.services[idx];
        if let Some(slot) = s.env.iter_mut().find(|e| e.split('=').next() == Some(key)) {
            *slot = line;
        } else {
            s.env.push(line);
        }
    }
    for p in ports {
        let new = PortSpec::parse(p).with_context(|| format!("puerto «{p}»"))?;
        let s = &mut plan.services[primary];
        let pos = s.ports.iter().position(|x| PortSpec::parse(x).map(|o| o.key() == new.key()).unwrap_or(false));
        match pos {
            Some(i) => s.ports[i] = p.clone(),
            None => s.ports.push(p.clone()),
        }
    }
    if let Some(img) = image {
        plan.services[primary].image = img.to_string();
    }
    if let Some(t) = tag {
        let s = &mut plan.services[primary];
        if s.image.is_empty() {
            bail!("esta plantilla no define imagen: usa --image");
        }
        let (repo, _) = split_image_tag(&s.image);
        s.image = format!("{repo}:{t}");
    }
    Ok(())
}

/// Imprime la unidad de systemd (de usuario) o la línea de cron que respalda todo cada día. No instala nada:
/// la activas tú, con las instrucciones que se imprimen.
pub fn print_schedule(at: &str, keep: usize, volumes: bool, format: &str) -> Result<()> {
    let (h, m) = at
        .split_once(':')
        .and_then(|(h, m)| Some((h.parse::<u8>().ok()?, m.parse::<u8>().ok()?)))
        .filter(|(h, m)| *h < 24 && *m < 60)
        .ok_or_else(|| anyhow!("hora inválida «{at}»: usa HH:MM"))?;
    let exe = std::env::current_exe()?.display().to_string();
    let vol = if volumes { " --volumes" } else { "" };
    let cmd = format!("{exe} backup --all{vol} --keep {keep} --yes");
    match format {
        "systemd" => {
            println!("# ~/.config/systemd/user/dockyard-backup.service");
            println!("[Unit]\nDescription=dockyard: respaldo diario\n\n[Service]\nType=oneshot\nExecStart={cmd}\n");
            println!("# ~/.config/systemd/user/dockyard-backup.timer");
            println!("[Unit]\nDescription=dockyard: respaldo diario a las {h:02}:{m:02}\n\n[Timer]\nOnCalendar=*-*-* {h:02}:{m:02}:00\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n");
            println!("# Para activarlo:");
            println!("#   systemctl --user daemon-reload && systemctl --user enable --now dockyard-backup.timer");
            println!("# Si usas DOCKYARD_BACKUPS o DOCKER_HOST, añádelos con Environment= en el servicio.");
        }
        "cron" => println!("{m} {h} * * * {cmd}"),
        other => bail!("formato desconocido «{other}»: usa systemd o cron"),
    }
    Ok(())
}
