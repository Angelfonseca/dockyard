//! Diagnóstico del entorno (`dockyard doctor`).

use super::*;

/// Comando de instalación de Docker, Compose y Git según la distribución (campos ID e ID_LIKE de /etc/os-release).
pub(super) fn install_hint(os_release: &str) -> &'static str {
    let mut ids = String::new();
    for l in os_release.lines() {
        if let Some(v) = l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")) {
            ids.push(' ');
            ids.push_str(v.trim_matches('"'));
        }
    }
    let has = |x: &str| ids.split_whitespace().any(|w| w == x);
    if has("arch") {
        "sudo pacman -S --needed docker docker-compose git"
    } else if has("alpine") {
        "sudo apk add docker docker-cli-compose git"
    } else if has("fedora") || has("rhel") || has("centos") {
        "sudo dnf install podman-docker docker-compose git   (o Docker CE del repositorio oficial)"
    } else if has("suse") {
        "sudo zypper install docker docker-compose git"
    } else if has("debian") || has("ubuntu") {
        "sudo apt install docker.io docker-compose-v2 git   (Debian: docker-compose; o Docker CE del repositorio oficial)"
    } else {
        "instala docker, docker compose y git con el gestor de tu distribución"
    }
}

/// ¿Puede este usuario escribir en el socket del daemon? `None` si el daemon no es un socket local (tcp, ssh).
pub(super) fn socket_writable(host: &str) -> Option<bool> {
    let path = host.strip_prefix("unix://")?;
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: `c` es una cadena terminada en NUL que vive durante la llamada.
    Some(unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
}

/// WSL2: el daemon tiene que correr dentro de WSL (Docker Desktop con integración o Docker instalado en la distro).
pub(super) fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version").map(|v| v.to_lowercase().contains("microsoft")).unwrap_or(false)
}

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
    if socket_writable(&engine.host) == Some(false) {
        println!("✘ Permisos       tu usuario no puede escribir en el socket de Docker: sudo usermod -aG docker $USER y vuelve a iniciar sesión");
    }
    if is_wsl() {
        println!("· WSL2           el daemon debe correr dentro de WSL (Docker Desktop con integración o Docker en la distro)");
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
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    println!("  Instalar aquí    {}", install_hint(&os));
    if engine.ssh_target().is_some() {
        println!("{} ssh              shell del servidor", opt(which("ssh").is_some()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_hint_follows_the_distribution() {
        assert!(install_hint("NAME=\"CachyOS\"\nID=cachyos\nID_LIKE=arch").contains("pacman"));
        assert!(install_hint("ID=ubuntu\nID_LIKE=debian").contains("apt"));
        assert!(install_hint("ID=fedora").contains("dnf"));
        assert!(install_hint("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"").contains("zypper"));
        assert!(install_hint("ID=alpine").contains("apk"));
        assert!(install_hint("ID=desconocida").contains("gestor de tu distribución"));
    }

    #[test]
    fn socket_check_only_for_local_sockets() {
        assert_eq!(socket_writable("tcp://10.0.0.2:2375"), None);
        assert_eq!(socket_writable("ssh://u@host"), None);
        assert_eq!(socket_writable("unix:///no/existe/docker.sock"), Some(false));
    }
}
