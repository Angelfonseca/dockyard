//! Diagnóstico del entorno (`dockyard doctor`).

use super::*;

pub(super) async fn doctor(engine: &Engine, cfg: &Config, catalog: &Catalog) {
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
