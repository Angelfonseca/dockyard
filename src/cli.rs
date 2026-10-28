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

#[derive(Parser)]
#[command(
    name = "dockyard",
    version,
    about = "Gestor de Docker ligero: TUI con catálogo de plantillas, edición de variables, logs, métricas y respaldos",
    long_about = "Sin subcomando abre la interfaz interactiva. Los subcomandos sirven para scripts y tareas rápidas."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Lista contenedores
    Ps {
        /// Incluir los detenidos
        #[arg(short, long)]
        all: bool,
    },
    /// CPU, memoria y red por contenedor (muestra de 1 segundo)
    Stats,
    /// Muestra los logs de un contenedor
    Logs {
        name: String,
        /// Seguir en vivo
        #[arg(short, long)]
        follow: bool,
        /// Últimas N líneas
        #[arg(short = 'n', long, default_value_t = 100)]
        tail: u32,
    },
    /// Lista las plantillas, o muestra el detalle de una
    Catalog { id: Option<String> },
    /// Instala una plantilla del catálogo
    Up {
        template: String,
        /// Nombre del stack (por defecto, el id de la plantilla)
        #[arg(short, long)]
        name: Option<String>,
        /// Variable CLAVE=valor (o rol.CLAVE=valor). Repetible
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Puerto host:contenedor[/udp] del servicio principal. Repetible
        #[arg(short = 'p', long = "port")]
        port: Vec<String>,
        /// Etiqueta de la imagen principal (ej. 16-alpine)
        #[arg(long)]
        tag: Option<String>,
        /// Imagen completa del servicio principal (útil con la plantilla custom)
        #[arg(long)]
        image: Option<String>,
        /// Solo mostrar lo que se haría
        #[arg(long)]
        dry_run: bool,
        /// No pedir confirmación
        #[arg(short, long)]
        yes: bool,
    },
    /// Ver o cambiar las variables de entorno de un contenedor (lo recrea al cambiar)
    Env {
        name: String,
        /// CLAVE=valor a establecer. Repetible
        #[arg(long = "set")]
        set: Vec<String>,
        /// CLAVE a quitar. Repetible
        #[arg(long = "unset")]
        unset: Vec<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Actualiza un contenedor: imagen nueva, o git pull + rebuild, o compose pull; recrea si cambió
    Update { name: String },
    /// Elimina un stack completo, de dockyard o de Compose (contenedores y redes; conserva volúmenes salvo --volumes)
    Down {
        stack: String,
        #[arg(long)]
        volumes: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Respaldo de una base de datos (contenedor) o de un volumen
    Backup { target: String },
    /// Restaura un respaldo: SQL/archivo en un contenedor de base de datos, o .tar.gz en un volumen
    Restore {
        /// Contenedor de base de datos o volumen
        target: String,
        /// Archivo a restaurar (por defecto, el respaldo más reciente de ese destino)
        file: Option<PathBuf>,
        /// Volumen: borrar su contenido antes de extraer
        #[arg(long)]
        clean: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Inicia contenedores
    Start { names: Vec<String> },
    /// Detiene contenedores
    Stop { names: Vec<String> },
    /// Reinicia contenedores
    Restart { names: Vec<String> },
    /// Elimina contenedores (conserva volúmenes)
    Rm {
        names: Vec<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Ejecuta un comando dentro de un contenedor (sin terminal; para scripts)
    Exec {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        cmd: Vec<String>,
    },
    /// Shell interactiva dentro de un contenedor (no necesita el CLI de docker)
    Shell {
        name: String,
        /// Programa a ejecutar en vez de bash/sh
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Contraseñas y tokens de un contenedor o de todo su stack
    Creds { target: String },
    /// Uso de disco de Docker: imágenes, contenedores, volúmenes y caché de construcción
    Df,
    /// Libera espacio (pide confirmación mostrando cuánto se recupera)
    Prune {
        /// Contenedores detenidos
        #[arg(long)]
        containers: bool,
        /// Imágenes que ningún contenedor usa
        #[arg(long)]
        images: bool,
        /// Caché de construcción
        #[arg(long)]
        build: bool,
        /// Redes sin uso
        #[arg(long)]
        networks: bool,
        /// Volúmenes huérfanos (¡borra datos!)
        #[arg(long)]
        volumes: bool,
        /// Contenedores, imágenes, caché y redes (los volúmenes hay que pedirlos aparte)
        #[arg(long)]
        all: bool,
        /// Solo lo que tenga esta etiqueta (clave o clave=valor)
        #[arg(long)]
        label: Option<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Volúmenes: listar con tamaño, explorar, clonar, editar archivos, borrar
    Vol {
        #[command(subcommand)]
        cmd: VolCmd,
    },
    /// Edita un archivo de un contenedor (activo o detenido) con tu $EDITOR
    Edit { name: String, path: String },
    /// Proyectos de Docker Compose (necesita docker compose o docker-compose)
    Compose {
        #[command(subcommand)]
        cmd: ComposeCmd,
    },
    /// Trae un repositorio Git y lo despliega (compose.yaml o Dockerfile)
    Git {
        url: String,
        /// Rama o etiqueta
        #[arg(short = 'r', long = "ref")]
        git_ref: Option<String>,
        /// Nombre del despliegue (por defecto, el del repositorio)
        #[arg(short, long)]
        name: Option<String>,
        /// Carpeta dentro del repositorio
        #[arg(long)]
        subdir: Option<String>,
        /// Variable CLAVE=valor (con Dockerfile). Repetible
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Puerto host:contenedor (con Dockerfile). Repetible
        #[arg(short = 'p', long = "port")]
        port: Vec<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Diagnóstico del entorno
    Doctor,
    /// Panel web ligero (estado, contenedores, logs, espacio)
    Web {
        /// Dirección donde escuchar, p. ej. la IP de Tailscale: 100.x.y.z:7681
        #[arg(short, long, default_value = "127.0.0.1:7681")]
        listen: String,
    },
}

#[derive(Subcommand)]
pub enum VolCmd {
    /// Lista volúmenes con su tamaño y quién los usa
    Ls,
    /// Carpetas más pesadas y contenido de un volumen
    Du { name: String },
    /// Copia exacta de un volumen en uno nuevo
    Clone { src: String, dst: String },
    /// Edita un archivo dentro de un volumen
    Edit { name: String, path: String },
    /// Borra volúmenes (no deben estar en uso)
    Rm {
        names: Vec<String>,
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ComposeCmd {
    /// Levanta un proyecto (archivo o carpeta con compose.yaml)
    Up {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Nombre del proyecto (por defecto, el de la carpeta)
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Descarga imágenes nuevas y recrea lo que cambió
    Pull {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Elimina un proyecto (por API: no necesita el archivo)
    Down {
        project: String,
        #[arg(long)]
        volumes: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Contenedores de un proyecto
    Ps { project: String },
}

fn table(rows: &[Vec<String>]) {
    if rows.is_empty() {
        return;
    }
    let cols = rows[0].len();
    let mut w = vec![0usize; cols];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == cols {
                line.push_str(c);
            } else {
                line.push_str(&format!("{:<width$}  ", c, width = w[i]));
            }
        }
        println!("{}", line.trim_end());
    }
}

fn confirm(msg: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    eprint!("{msg} [s/N] ");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(matches!(s.trim().to_lowercase().as_str(), "s" | "si" | "sí" | "y" | "yes"))
}

fn progress_printer() -> impl FnMut(String) + Send {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let mut last = String::new();
    move |s: String| {
        // sin terminal (scripts, logs) la barra de progreso solo ensucia
        if tty && s != last {
            eprint!("\r\x1b[K{s}");
            std::io::stderr().flush().ok();
            last = s;
        }
    }
}

fn clear_progress() {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K");
    }
}

fn line_printer() -> impl FnMut(String) + Send {
    |s: String| eprintln!("{s}")
}

async fn find(engine: &Engine, name: &str) -> Result<CRow> {
    let all = engine.list_containers().await?;
    if let Some(c) = all.iter().find(|c| c.name == name) {
        return Ok(c.clone());
    }
    let matches: Vec<&CRow> = all.iter().filter(|c| c.id.starts_with(name) || c.name.starts_with(name)).collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => bail!("no existe el contenedor «{name}»"),
        _ => bail!("«{name}» es ambiguo: {}", matches.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

async fn volume_exists(engine: &Engine, name: &str) -> Result<bool> {
    Ok(engine.list_volumes(&[]).await?.iter().any(|v| v.name == name))
}

fn usage_row(name: &str, u: &Usage) -> Vec<String> {
    if !u.known {
        return vec![name.into(), "-".into(), "-".into(), "n/d".into(), "n/d".into()];
    }
    vec![name.into(), u.count.to_string(), u.active.to_string(), human_bytes(u.size), human_bytes(u.reclaimable)]
}

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
            let mut freed = 0u64;
            if c {
                let (k, s) = engine.prune_containers(label).await?;
                freed += s;
                println!("✔ {k} contenedores eliminados");
            }
            if i {
                let (k, s) = engine.prune_images(label).await?;
                freed += s;
                println!("✔ {k} imágenes eliminadas");
            }
            if b {
                if label.is_some() {
                    println!("  (la caché de construcción no se filtra por etiqueta; se omitió)");
                } else {
                    freed += engine.prune_build(None).await?;
                    println!("✔ caché de construcción limpia");
                }
            }
            if n {
                println!("✔ {} redes eliminadas", engine.prune_networks(label).await?);
            }
            if volumes {
                let mut ok = 0;
                for v in &orphans {
                    match engine.remove_volume(v).await {
                        Ok(()) => ok += 1,
                        Err(e) => eprintln!("✘ {v}: {e}"),
                    }
                }
                println!("✔ {ok} volúmenes eliminados");
            }
            println!("Espacio liberado: {}", human_bytes(freed));
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
        Cmd::Git { url, git_ref, name, subdir, env, port, yes } => {
            let name = name.unwrap_or_else(|| repo_name(&url));
            let src = ext::GitSource { url, git_ref, name, subdir };
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
        Cmd::Doctor => doctor(&engine, &cfg, &catalog).await,
        Cmd::Web { listen } => crate::web::serve(engine, &listen, cfg, catalog).await?,
    }
    Ok(())
}

/// Nombre por defecto a partir de la URL: «https://x/y/mi-app.git» → «mi-app».
pub fn repo_name(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("app");
    let n = last.trim_end_matches(".git");
    let n: String = n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    if n.is_empty() { "app".into() } else { n }
}

fn compose_target(path: &Path, name: Option<String>) -> Result<(String, PathBuf)> {
    let file = ext::find_compose_file(path).ok_or_else(|| anyhow!("no encontré compose.yaml ni docker-compose.yml en {}", path.display()))?;
    let file = std::fs::canonicalize(&file)?;
    let project = match name {
        Some(n) => ext::project_name(&n),
        None => ext::project_name(&file.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().to_string()).unwrap_or_default()),
    };
    Ok((project, file))
}

async fn down(engine: &Engine, stack: &str, volumes: bool, yes: bool) -> Result<()> {
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

async fn deploy_plan(engine: &Engine, plan: Plan, dry_run: bool, yes: bool) -> Result<()> {
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
    let rep = engine.deploy(&plan, &mut prog).await;
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
async fn edit_file(engine: &Engine, container: &str, path: &str) -> Result<bool> {
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

fn report_edit(saved: bool, path: &str) {
    if saved {
        println!("✔ {path} guardado");
    } else {
        println!("Sin cambios en {path}");
    }
}

async fn each(engine: &Engine, names: &[String], act: Act, done: &str) -> Result<()> {
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

async fn sample_all(engine: &Engine, list: &[CRow]) -> std::collections::HashMap<String, crate::engine::RawStat> {
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

async fn doctor(engine: &Engine, cfg: &Config, catalog: &Catalog) {
    let ok = |b: bool| if b { "✔" } else { "✘" };
    let opt = |b: bool| if b { "✔" } else { "·" };
    println!("  Daemon           {} ({})", engine.host, engine.source);
    match timed(10, engine.version()).await {
        Ok(v) => {
            let api = engine.d.client_version();
            println!("{} Conexión         Docker {v} · API {api}", ok(true));
            if let Ok(info) = timed(10, async { Ok(engine.d.info().await?) }).await {
                println!(
                    "  Sistema          {} · datos en {} · logs: {}",
                    info.operating_system.unwrap_or_default(),
                    info.docker_root_dir.unwrap_or_default(),
                    info.logging_driver.unwrap_or_default()
                );
            }
        }
        Err(e) => println!("{} Conexión         {e:#}", ok(false)),
    }
    match &engine.log_rotation {
        Some((s, n)) => println!("  Rotación de logs {s} × {n} (contenedores nuevos o recreados)"),
        None => println!("  Rotación de logs desactivada"),
    }
    println!("{} Plantillas       {} cargadas", ok(true), catalog.templates.len());
    println!("  Config           {}", crate::util::config_dir().display());
    let bdir = cfg.backups_dir();
    let writable = std::fs::create_dir_all(&bdir).is_ok() && {
        let probe = bdir.join(format!(".dockyard-{}", rand_alnum(6)));
        let w = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        w
    };
    println!("{} Respaldos        {}{}", ok(writable), bdir.display(), if writable { "" } else { " (sin permiso de escritura)" });
    println!("  Zona horaria     {}", cfg.tz.clone().unwrap_or_else(|| "(no detectada)".into()));
    println!("\nOpcionales (solo para las funciones que los usan):");
    println!("{} docker (CLI)     construir con BuildKit", opt(which("docker").is_some()));
    println!(
        "{} compose          {}",
        opt(ext::compose_cmd().is_some()),
        ext::compose_cmd().map(|c| c.join(" ")).unwrap_or_else(|| "proyectos Compose: instala docker-compose".into())
    );
    println!("{} git              traer desde repositorios", opt(which("git").is_some()));
    println!(
        "{} editor           {}",
        opt(ext::editor().is_some()),
        ext::editor().map(|e| e.join(" ")).unwrap_or_else(|| "editar archivos: define $EDITOR o instala nano".into())
    );
    if engine.ssh_target().is_some() {
        println!("{} ssh              shell del servidor", opt(which("ssh").is_some()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names() {
        assert_eq!(repo_name("https://github.com/u/mi-app.git"), "mi-app");
        assert_eq!(repo_name("git@github.com:u/otra.git"), "otra");
        assert_eq!(repo_name("https://x.org/a/b/"), "b");
        assert_eq!(repo_name(""), "app");
    }
}
