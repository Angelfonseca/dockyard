//! Subcomandos de línea de comandos (para scripts y uso rápido sin TUI).

use crate::catalog::{Catalog, Plan, PortSpec};
use crate::config::Config;
use crate::engine::{Act, Engine, UpdateOutcome, timed};
use crate::ext;
use crate::model::*;
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod args;
mod output;
mod ops;
mod doctor;
#[cfg(test)]
mod tests;

pub use args::*;
pub use ops::*;
use output::*;
use doctor::*;

pub async fn run(cmd: Cmd) -> Result<()> {
    let (cfg, _) = Config::load();
    let mut engine = Engine::connect().await?;
    engine.log_rotation = cfg.log_rotation();
    let (catalog, _) = Catalog::load();
    if !matches!(cmd, Cmd::Catalog { .. } | Cmd::Doctor) {
        timed(10, engine.version())
            .await
            .with_context(|| format!("no se puede hablar con Docker en {} (¿está corriendo? ¿tu usuario está en el grupo docker?)", engine.host))?;
    }
    match cmd {
        Cmd::Ps { all } => {
            let mut rows = vec![vec!["NOMBRE".into(), "ESTADO".into(), "IMAGEN".into(), "PUERTOS".into()]];
            for c in engine.list_containers().await? {
                if !all && !c.is_up() {
                    continue;
                }
                rows.push(vec![c.name.clone(), c.status.clone(), c.image.clone(), c.ports.clone()]);
            }
            table(&rows);
        }
        Cmd::Stats => {
            let list: Vec<CRow> = engine.list_containers().await?.into_iter().filter(|c| c.state == St::Running).collect();
            let a = sample_all(&engine, &list).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            let b = sample_all(&engine, &list).await;
            let mut rows_data: Vec<(String, f64, u64, u64, f64, f64)> = Vec::new();
            for c in &list {
                if let (Some(x), Some(y)) = (a.get(&c.id), b.get(&c.id)) {
                    let dc = y.cpu_total.saturating_sub(x.cpu_total) as f64;
                    let ds = y.sys_cpu.saturating_sub(x.sys_cpu) as f64;
                    let cpu = if ds > 0.0 { dc / ds * y.online as f64 * 100.0 } else { 0.0 };
                    rows_data.push((c.name.clone(), cpu, y.mem, y.limit, y.rx.saturating_sub(x.rx) as f64, y.tx.saturating_sub(x.tx) as f64));
                }
            }
            rows_data.sort_by(|p, q| q.1.partial_cmp(&p.1).unwrap_or(std::cmp::Ordering::Equal));
            let mut rows = vec![vec!["NOMBRE".into(), "CPU".into(), "MEMORIA".into(), "LÍMITE".into(), "RED ↓".into(), "RED ↑".into()]];
            for (n, cpu, mem, lim, rx, tx) in rows_data {
                rows.push(vec![n, format!("{cpu:.1}%"), human_bytes(mem), if lim > 0 { human_bytes(lim) } else { "-".into() }, human_rate(rx), human_rate(tx)]);
            }
            table(&rows);
        }
        Cmd::Logs { name, follow, tail } => {
            let c = find(&engine, &name).await?;
            let mut s = Box::pin(engine.log_stream(&c.id, &tail.to_string(), follow, false));
            let mut out = std::io::stdout();
            while let Some(chunk) = s.next().await {
                let chunk = chunk?;
                if out.write_all(chunk.as_ref()).is_err() {
                    break;
                }
                out.flush().ok();
            }
        }
        Cmd::Catalog { id } => match id {
            None => {
                let mut rows = vec![vec!["ID".into(), "NOMBRE".into(), "CATEGORÍA".into(), "SERVICIOS".into()]];
                for t in &catalog.templates {
                    rows.push(vec![t.id.clone(), t.name.clone(), t.category.clone(), t.services.len().to_string()]);
                }
                table(&rows);
                println!("\nInstala con: dockyard up <id> [--name X] [-e CLAVE=valor] [-p host:contenedor]");
            }
            Some(id) => {
                let t = catalog.get(&id).ok_or_else(|| anyhow!("no hay plantilla «{id}» (usa `dockyard catalog`)"))?;
                println!("{}  [{}]\n{}\n", t.name, t.category, t.description);
                let plan = Plan::build(t, &t.id, cfg.tz.as_deref(), &cfg.restart());
                print_plan(&plan);
                if !t.note.is_empty() {
                    println!("Nota: {}", t.note);
                }
            }
        },
        Cmd::Up { template, name, env, port, tag, image, dry_run, yes } => {
            let t = catalog.get(&template).ok_or_else(|| anyhow!("no hay plantilla «{template}» (usa `dockyard catalog`)"))?;
            let name = name.unwrap_or_else(|| t.id.clone());
            let mut plan = Plan::build(t, &name, cfg.tz.as_deref(), &cfg.restart());
            apply_overrides(&mut plan, &env, &port, tag.as_deref(), image.as_deref())?;
            deploy_plan(&engine, plan, dry_run, yes).await?;
        }
        Cmd::Env { name, set, unset, yes } => {
            let c = find(&engine, &name).await?;
            let (src, mut plan) = engine.load_edit(&c.id).await?;
            if set.is_empty() && unset.is_empty() {
                for e in &plan.services[0].env {
                    println!("{e}");
                }
                return Ok(());
            }
            let s = &mut plan.services[0];
            for k in &unset {
                s.env.retain(|e| e.split('=').next() != Some(k.as_str()));
            }
            for kv in &set {
                let Some((k, _)) = kv.split_once('=') else { bail!("«{kv}» debe ser CLAVE=valor") };
                if let Some(slot) = s.env.iter_mut().find(|e| e.split('=').next() == Some(k)) {
                    *slot = kv.clone();
                } else {
                    s.env.push(kv.clone());
                }
            }
            if !confirm(&format!("Se recreará «{}» con las variables cambiadas (datos y volúmenes se conservan). ¿Continuar?", c.name), yes)? {
                bail!("cancelado");
            }
            let mut prog = progress_printer();
            let r = engine.recreate(&src, &plan, &mut prog).await;
            eprintln!();
            r?;
            println!("✔ «{}» recreado", c.name);
        }
        Cmd::Update { name } => {
            let c = find(&engine, &name).await?;
            let mut prog = line_printer();
            match ext::update_container(&engine, &c.id, &mut prog).await? {
                UpdateOutcome::UpToDate => println!("«{}» ya está al día", c.name),
                UpdateOutcome::Updated => println!("✔ «{}» actualizado", c.name),
            }
        }
        Cmd::Down { stack, volumes, yes } => down(&engine, &stack, volumes, yes).await?,
        Cmd::Backup { target } => {
            let dir = cfg.backups_dir();
            let all = engine.list_containers().await?;
            if let Some(c) = all.iter().find(|c| c.name == target) {
                let Some((script, ext)) = catalog.dump_for_image(&c.image) else {
                    bail!("no conozco un comando de respaldo para {}; respalda sus volúmenes: dockyard backup <volumen>", c.image);
                };
                let (p, n) = engine.backup_dump(&c.id, &c.name, &script, &ext, &dir).await?;
                println!("✔ {} ({})", p.display(), human_bytes(n));
            } else if volume_exists(&engine, &target).await? {
                let mut prog = progress_printer();
                let r = engine.backup_volume(&target, &dir, &mut prog).await;
                eprintln!();
                let (p, n) = r?;
                println!("✔ {} ({})", p.display(), human_bytes(n));
            } else {
                bail!("«{target}» no es un contenedor ni un volumen");
            }
        }
        Cmd::Restore { target, file, clean, yes } => {
            let dir = cfg.backups_dir();
            let all = engine.list_containers().await?;
            if let Some(c) = all.iter().find(|c| c.name == target) {
                let Some((script, ext)) = catalog.restore_for_image(&c.image) else {
                    bail!("no conozco cómo restaurar en {}; si quieres restaurar un volumen, indica su nombre", c.image);
                };
                if !c.is_up() {
                    bail!("«{}» debe estar en ejecución para restaurar", c.name);
                }
                let file = match file {
                    Some(f) => f,
                    None => latest_backup(&dir, &c.name, &ext).ok_or_else(|| anyhow!("no hay respaldos de «{}» en {}", c.name, dir.display()))?,
                };
                if !confirm(&format!("Se cargará {} en la base de datos de «{}» (puede sobrescribir datos). ¿Continuar?", file.display(), c.name), yes)? {
                    bail!("cancelado");
                }
                let mut prog = progress_printer();
                let r = engine.exec_from_file(&c.id, &script, &file, &mut prog).await;
                eprintln!();
                r?;
                println!("✔ {} restaurado en «{}»", file.display(), c.name);
            } else {
                let file = match file {
                    Some(f) => f,
                    None => latest_backup(&dir, &target, "tar.gz").ok_or_else(|| anyhow!("no hay respaldos de «{target}» en {}", dir.display()))?,
                };
                let what = if clean { "se BORRARÁ su contenido y se extraerá" } else { "se extraerá (sobrescribiendo archivos)" };
                if !confirm(&format!("En el volumen «{target}» {what} {}. ¿Continuar?", file.display()), yes)? {
                    bail!("cancelado");
                }
                let mut prog = progress_printer();
                let r = engine.restore_volume(&target, &file, clean, &mut prog).await;
                eprintln!();
                r?;
                println!("✔ restaurado en «{target}»");
            }
        }
        Cmd::Start { names } => each(&engine, &names, Act::Start, "iniciado").await?,
        Cmd::Stop { names } => each(&engine, &names, Act::Stop, "detenido").await?,
        Cmd::Restart { names } => each(&engine, &names, Act::Restart, "reiniciado").await?,
        Cmd::Rm { names, yes } => {
            if !confirm(&format!("Se eliminarán: {}. ¿Continuar?", names.join(", ")), yes)? {
                bail!("cancelado");
            }
            each(&engine, &names, Act::Remove, "eliminado").await?
        }
        Cmd::Exec { name, cmd } => {
            let c = find(&engine, &name).await?;
            let (out, code) = engine.exec_collect(&c.id, cmd, Duration::from_secs(300)).await?;
            print!("{out}");
            if let Some(h) = crate::engine::missing_binary_hint(&out) {
                eprintln!("{h}");
            }
            if let Some(code) = code {
                if code != 0 {
                    std::process::exit(code as i32);
                }
            }
        }
        Cmd::Shell { name, cmd } => {
            let c = find(&engine, &name).await?;
            if !c.is_up() {
                bail!("«{}» no está en ejecución", c.name);
            }
            let cmd = if cmd.is_empty() { crate::shell::default_shell() } else { cmd };
            let code = crate::shell::interactive(&engine, &c.id, cmd).await?;
            if matches!(code, Some(126 | 127)) {
                eprintln!("{}", crate::engine::missing_binary_hint("executable file not found").unwrap_or_default());
            }
            if let Some(code) = code.filter(|c| *c != 0) {
                std::process::exit(code as i32);
            }
        }
        Cmd::Creds { target } => {
            let creds = engine.credentials(&target).await?;
            if creds.is_empty() {
                println!("«{target}» no tiene variables de contraseña/token");
            }
            for (c, k, v) in creds {
                println!("{c:<22} {k} = {v}");
            }
        }
        Cmd::Df => {
            let mut prog = progress_printer();
            let u = engine.disk_usage(&mut prog).await?;
            clear_progress();
            let mut rows = vec![vec!["TIPO".into(), "TOTAL".into(), "EN USO".into(), "TAMAÑO".into(), "RECUPERABLE".into()]];
            rows.push(usage_row("Imágenes", &u.images));
            rows.push(usage_row("Contenedores", &u.containers));
            rows.push(usage_row("Volúmenes", &u.volumes));
            rows.push(usage_row("Caché de build", &u.build));
            table(&rows);
            if let Some((root, used, total)) = &u.root {
                println!("\nDisco de Docker ({root}): {} de {} usados", human_bytes(*used), human_bytes(*total));
            }
            let mut vols: Vec<(&String, &u64)> = u.vol_sizes.iter().collect();
            vols.sort_by(|a, b| b.1.cmp(a.1));
            if !vols.is_empty() {
                println!("\nVolúmenes más grandes:");
                for (n, s) in vols.iter().take(10) {
                    println!("  {:>8}  {n}", human_bytes(**s));
                }
            }
        }
        Cmd::Prune { containers, images, build, networks, volumes, all, label, yes } => {
            let (c, i, b, n) = (containers || all, images || all, build || all, networks || all);
            if !(c || i || b || n || volumes) {
                bail!("elige qué limpiar: --containers --images --build --networks --volumes, o --all");
            }
            let label = label.as_deref();
            let orphans = if volumes { engine.orphan_volumes(label).await? } else { vec![] };
            let mut prog = progress_printer();
            let u = engine.disk_usage(&mut prog).await.unwrap_or_default();
            clear_progress();
            println!("Se limpiará{}:", label.map(|l| format!(" (solo con etiqueta {l})")).unwrap_or_default());
            // con filtro no hay estimación fiable: el uso de disco es global
            let est = |x: &Usage| if label.is_some() || !x.known { String::new() } else { format!(" (≈ {})", human_bytes(x.reclaimable)) };
            if c {
                println!("  • contenedores detenidos{}", est(&u.containers));
            }
            if i {
                println!("  • imágenes sin uso{}", est(&u.images));
            }
            if b {
                println!("  • caché de construcción{}", est(&u.build));
            }
            if n {
                println!("  • redes sin uso");
            }
            if volumes {
                println!("  • {} volúmenes huérfanos — SUS DATOS SE PIERDEN:", orphans.len());
                for v in orphans.iter().take(20) {
                    println!("      {v}");
                }
            }
            if !confirm("¿Continuar?", yes)? {
                bail!("cancelado");
            }
            let o = crate::ops::PruneOpts { containers: c, images: i, build: b, networks: n, volumes, label: label.map(String::from) };
            let r = crate::ops::prune(&engine, &o).await?;
            if c {
                println!("✔ {} contenedores eliminados", r.containers);
            }
            if i {
                println!("✔ {} imágenes eliminadas", r.images);
            }
            if b {
                if r.build_skipped {
                    println!("  (la caché de construcción no se filtra por etiqueta; se omitió)");
                } else {
                    println!("✔ caché de construcción limpia");
                }
            }
            if n {
                println!("✔ {} redes eliminadas", r.networks);
            }
            if volumes {
                println!("✔ {} volúmenes eliminados", r.volumes);
                if r.volumes_failed > 0 {
                    eprintln!("✘ {} volúmenes no se pudieron borrar (¿en uso?): `dockyard vol rm NOMBRE` muestra el motivo", r.volumes_failed);
                }
            }
            println!("Espacio liberado: {}", human_bytes(r.freed));
        }
        Cmd::Vol { cmd } => match cmd {
            VolCmd::Ls => {
                let all = engine.list_containers().await?;
                let vols = engine.list_volumes(&all).await?;
                let mut prog = progress_printer();
                let u = engine.disk_usage(&mut prog).await.unwrap_or_default();
                clear_progress();
                let mut rows = vec![vec!["NOMBRE".into(), "TAMAÑO".into(), "EN USO POR".into()]];
                for v in vols {
                    let size = u.vol_sizes.get(&v.name).map(|s| human_bytes(*s)).unwrap_or_else(|| "?".into());
                    let used = if v.used_by.is_empty() { "huérfano".into() } else { v.used_by.join(", ") };
                    rows.push(vec![v.name, size, used]);
                }
                table(&rows);
            }
            VolCmd::Du { name } => {
                let mut prog = progress_printer();
                let r = engine.volume_du(&name, &mut prog).await;
                clear_progress();
                print!("{}", r?);
            }
            VolCmd::Clone { src, dst } => {
                let mut prog = progress_printer();
                let r = engine.clone_volume(&src, &dst, &mut prog).await;
                eprintln!();
                r?;
                println!("✔ «{src}» copiado en «{dst}»");
            }
            VolCmd::Edit { name, path } => {
                if !volume_exists(&engine, &name).await? {
                    bail!("no existe el volumen «{name}»");
                }
                let mut prog = progress_printer();
                let helper = engine.create_helper(vec![format!("{name}:/data")], None, &mut prog).await?;
                let inner = format!("/data/{}", path.trim_start_matches('/'));
                let r = edit_file(&engine, &helper, &inner).await;
                engine.remove_helper(&helper).await;
                report_edit(r?, &path);
            }
            VolCmd::Rm { names, yes } => {
                if names.is_empty() {
                    bail!("indica al menos un volumen");
                }
                if !confirm(&format!("Se borrarán los volúmenes {} y TODOS sus datos. ¿Continuar?", names.join(", ")), yes)? {
                    bail!("cancelado");
                }
                for n in names {
                    engine.remove_volume(&n).await.with_context(|| format!("«{n}» (¿está en uso?)"))?;
                    println!("✔ {n} eliminado");
                }
            }
        },
        Cmd::Edit { name, path } => {
            let c = find(&engine, &name).await?;
            let saved = edit_file(&engine, &c.id, &path).await?;
            report_edit(saved, &path);
            if saved && c.is_up() {
                println!("  Reinicia el contenedor para aplicar: dockyard restart {}", c.name);
            }
        }
        Cmd::Compose { cmd } => match cmd {
            ComposeCmd::Up { path, name } => {
                let (project, file) = compose_target(&path, name)?;
                let mut out = line_printer();
                ext::compose(&engine, &project, &[file], &["up", "-d", "--remove-orphans"], &mut out).await?;
                println!("✔ proyecto «{project}» levantado");
            }
            ComposeCmd::Pull { path, name } => {
                let (project, file) = compose_target(&path, name)?;
                let mut out = line_printer();
                ext::compose(&engine, &project, std::slice::from_ref(&file), &["pull"], &mut out).await?;
                ext::compose(&engine, &project, &[file], &["up", "-d", "--remove-orphans"], &mut out).await?;
                println!("✔ proyecto «{project}» actualizado");
            }
            ComposeCmd::Down { project, volumes, yes } => down(&engine, &project, volumes, yes).await?,
            ComposeCmd::Ps { project } => {
                let mut rows = vec![vec!["NOMBRE".into(), "ESTADO".into(), "IMAGEN".into(), "PUERTOS".into()]];
                for c in engine.list_containers().await?.into_iter().filter(|c| c.stack.as_deref() == Some(project.as_str())) {
                    rows.push(vec![c.name, c.status, c.image, c.ports]);
                }
                if rows.len() == 1 {
                    bail!("no hay contenedores del proyecto «{project}»");
                }
                table(&rows);
            }
        },
        Cmd::Git { url, git_ref, name, subdir, user, token, env, env_file, port, yes } => {
            if token.is_some() {
                eprintln!("aviso: --token queda visible en `ps` y en el historial de la shell; mejor usa DOCKYARD_GIT_TOKEN");
            }
            let name = name.unwrap_or_else(|| repo_name(&ext::split_auth(&url).0));
            let mut src = ext::GitSource::new(&url, git_ref, name, subdir, user, token);
            let mut text = match &env_file {
                Some(p) => std::fs::read_to_string(p).with_context(|| format!("no se pudo leer {}", p.display()))?,
                None => String::new(),
            };
            for e in &env {
                text.push_str(&format!("\n{e}"));
            }
            if !text.trim().is_empty() {
                src.env.push(ext::EnvFile { name: ".env".into(), text });
            }
            let tmpl = catalog.get("custom").cloned().unwrap_or_default();
            let mut out = line_printer();
            match ext::git_deploy(&engine, &src, &tmpl, cfg.tz.as_deref(), &cfg.restart(), &mut out).await? {
                ext::GitOutcome::Compose => println!("✔ «{}» levantado con Compose", src.name),
                ext::GitOutcome::Image(plan) => {
                    let mut plan = *plan;
                    apply_overrides(&mut plan, &env, &port, None, None)?;
                    deploy_plan(&engine, plan, false, yes).await?;
                }
            }
        }
        Cmd::Tunnel { cmd } => match cmd {
            TunnelCmd::Up { name, port, provider, hours, open, yes } => {
                let c = find(&engine, &name).await?;
                let provider = crate::tunnel::Provider::parse(&provider)?;
                let ports = engine.tunnel_ports(&c.id).await?;
                let port = match (port, ports.as_slice()) {
                    (Some(p), _) => p,
                    (None, [p]) => *p,
                    (None, []) => bail!("«{}» no declara puertos TCP", c.name),
                    (None, many) => bail!("«{}» tiene varios puertos ({}); elige uno con -p", c.name, many.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")),
                };
                let lim = if hours == 0 { "sin límite".to_string() } else { format!("{hours} h") };
                eprintln!("Esto publica {}:{port} en INTERNET: cualquiera con el enlace podrá entrar ({lim}).", c.name);
                if !confirm("¿Abrir el túnel?", yes)? {
                    bail!("cancelado");
                }
                let o = crate::tunnel::Opts { provider, port, hours, authtoken: None, basic_auth: !open };
                let mut p = progress_printer();
                let t = engine.tunnel_start(&c.id, &o, &mut p).await?;
                clear_progress();
                println!("✔ {}", t.url.as_deref().unwrap_or("(sin URL)"));
                if let Some(a) = &t.auth {
                    println!("  acceso: {a}");
                } else if provider == crate::tunnel::Provider::Cloudflare {
                    eprintln!("  aviso: cloudflared no pide contraseña; úsalo con datos de prueba");
                }
                println!("  cerrar: dockyard tunnel down {}", c.name);
            }
            TunnelCmd::Ls => {
                let list = engine.tunnels().await?;
                if list.is_empty() {
                    println!("no hay túneles abiertos");
                }
                let now = now_secs();
                for t in list {
                    let left = if t.expires == 0 { "sin límite".into() } else { format!("{} min", ((t.expires - now) / 60).max(0)) };
                    println!("{:<24} :{:<5} {:<10} {:<11} {}", t.target, t.port, t.provider, left, t.url.unwrap_or_else(|| "—".into()));
                }
            }
            TunnelCmd::Down { name } => {
                let list = engine.tunnels().await?;
                let hits: Vec<_> = list.iter().filter(|t| t.target == name || t.name == name).collect();
                if hits.is_empty() {
                    bail!("no hay túneles de «{name}»");
                }
                for t in hits {
                    engine.tunnel_stop(&t.name).await?;
                    println!("✔ cerrado {}", t.name);
                }
            }
        },
        Cmd::Doctor => doctor(&engine, &cfg, &catalog).await,
        Cmd::Web { listen, rotate_token, allow_host } => {
            crate::web::serve(engine, &listen, cfg, catalog, rotate_token, allow_host).await?
        }
        Cmd::WebLink { hours, listen } => crate::web::print_link(&listen, hours)?,
    }
    Ok(())
}

