//! Proyectos Compose: buscar el archivo, nombrar el proyecto y ejecutar comandos.

use super::*;

// ───────────────────────────── Compose ─────────────────────────────

/// Archivo Compose: la ruta misma si es un archivo, o el primero de COMPOSE_FILES en la carpeta.
pub fn find_compose_file(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    COMPOSE_FILES.iter().map(|f| path.join(f)).find(|p| p.is_file())
}

/// Nombre de proyecto válido para Compose (minúsculas, números, - y _).
pub fn project_name(s: &str) -> String {
    let n: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let n = n.trim_matches(|c| c == '-' || c == '_').to_string();
    if n.is_empty() { "app".into() } else { n }
}

/// `docker compose -p <proyecto> -f <archivos…> <args…>` en la carpeta del primer archivo.
pub async fn compose(engine: &Engine, project: &str, files: &[PathBuf], args: &[&str], out: Out<'_>) -> Result<()> {
    let Some(base) = compose_cmd() else {
        bail!("Compose necesita «docker compose» (plugin) o «docker-compose»; en Arch: sudo pacman -S docker-compose");
    };
    let mut cmd = tokio::process::Command::new(&base[0]);
    cmd.args(&base[1..]).arg("-p").arg(project);
    for f in files {
        cmd.arg("-f").arg(f);
    }
    cmd.args(args);
    if let Some(dir) = files.first().and_then(|f| f.parent()) {
        cmd.current_dir(dir);
    }
    docker_env(&mut cmd, engine);
    out(format!("$ {} -p {project} {}", base.join(" "), args.join(" ")));
    run_lines(cmd, "docker compose", out).await
}

/// Proyecto, servicio, archivos y carpeta de un contenedor creado por Compose (de sus etiquetas).
pub fn compose_origin(labels: &HashMap<String, String>) -> Option<(String, String, Vec<PathBuf>)> {
    let project = labels.get("com.docker.compose.project")?.clone();
    let service = labels.get("com.docker.compose.service").cloned().unwrap_or_default();
    let files: Vec<PathBuf> = labels
        .get("com.docker.compose.project.config_files")
        .map(|f| f.split(',').filter(|x| !x.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default();
    Some((project, service, files))
}

