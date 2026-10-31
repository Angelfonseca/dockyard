//! Actualizar un contenedor desde su imagen, su repo Git o su proyecto Compose.

use super::*;

// ───────────────────────────── Actualizar (imagen, Git o Compose) ─────────────────────────────

/// Actualiza un contenedor según su origen: Git (pull + build + recrear), Compose (pull + up) o imagen.
pub async fn update_container(engine: &Engine, id: &str, progress: Prog<'_>) -> Result<UpdateOutcome> {
    let i = engine.d.inspect_container(id, None::<InspectContainerOptions>).await?;
    let labels = i.config.as_ref().and_then(|c| c.labels.clone()).unwrap_or_default();
    let mut out = |s: String| progress(s);
    if let Some(dir) = labels.get("dockyard.git.dir") {
        let url = labels.get("dockyard.git.url").cloned().unwrap_or_default();
        let name = Path::new(dir).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let src = GitSource::new(&url, labels.get("dockyard.git.ref").cloned(), name, labels.get("dockyard.git.subdir").cloned(), None, None);
        // la etiqueta la puede escribir cualquier compose: solo se acepta una carpeta propia de dockyard
        let dir = PathBuf::from(dir);
        let own = data_dir().join("src");
        let real = dir.canonicalize().map_err(|_| anyhow!("no existe {}", dir.display()))?;
        if real.parent() != own.canonicalize().ok().as_deref() {
            bail!("la carpeta {} no es de dockyard; no se actualiza", dir.display());
        }
        git_sync(&src, &dir, &mut out).await?;
        let ctx = repo_ctx(&dir, src.subdir.as_deref())?;
        let image = i.config.as_ref().and_then(|c| c.image.clone()).unwrap_or_default();
        // antes de construir: la imagen vieja deja de poder inspeccionarse al reetiquetarla
        let (edit_src, plan) = engine.load_edit(id).await?;
        let before = match engine.image_fingerprint(i.image.as_deref().unwrap_or_default()).await {
            Some(f) => Some(f),
            None => engine.image_fingerprint(&image).await,
        };
        build_image(engine, &ctx, &image, &mut out).await?;
        if before.is_some() && before == engine.image_fingerprint(&image).await {
            return Ok(UpdateOutcome::UpToDate);
        }
        engine.recreate(&edit_src, &plan, &mut out).await?;
        return Ok(UpdateOutcome::Updated);
    }
    if let Some((project, service, files)) = compose_origin(&labels) {
        if compose_cmd().is_some() && !files.is_empty() && files.iter().all(|f| f.is_file()) {
            let svc: Vec<&str> = if service.is_empty() { vec![] } else { vec![service.as_str()] };
            compose(engine, &project, &files, &[&["pull"][..], &svc].concat(), &mut out).await?;
            compose(engine, &project, &files, &[&["up", "-d"][..], &svc].concat(), &mut out).await?;
            return Ok(UpdateOutcome::Updated);
        }
    }
    engine.update(id, &mut out).await
}

