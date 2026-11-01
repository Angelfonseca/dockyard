//! Continuación de Git: inspección del repo, construcción de imágenes y despliegue.

use super::*;

/// Trae el repositorio y dice qué es y qué archivos de variables le faltan, para pedirlos antes de desplegar.
pub async fn git_inspect(src: &GitSource, out: Out<'_>) -> Result<GitInspect> {
    let dir = git_fetch(src, out).await?;
    let ctx = repo_ctx(&dir, src.subdir.as_deref())?;
    let example = ENV_EXAMPLES.iter().find_map(|n| read_small(&ctx.join(n)));
    if let Some(f) = find_compose_file(&ctx) {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let mut env_files: Vec<(String, Option<String>)> = compose_env_files(&text)
            .into_iter()
            .filter(|p| !ctx.join(p).exists())
            .map(|p| {
                // «.env» ↔ «.env.example», «api/.env» ↔ «api/.env.example»
                let ex = ["example", "sample", "template", "dist"].iter().find_map(|s| read_small(&ctx.join(format!("{p}.{s}"))));
                let ex = ex.or_else(|| if p == ".env" { example.clone() } else { None });
                (p, ex)
            })
            .collect();
        // sin env_file declarado, un .env opcional sirve para las `${VARIABLES}` del compose
        if env_files.is_empty() && example.is_some() && !ctx.join(".env").exists() {
            env_files.push((".env".into(), example.clone()));
        }
        return Ok(GitInspect { kind: "compose", env_files, env_example: example });
    }
    if ctx.join("Dockerfile").is_file() {
        return Ok(GitInspect { kind: "dockerfile", env_files: vec![], env_example: example });
    }
    bail!("no encontré compose.yaml ni Dockerfile en {}", ctx.display())
}

/// Traduce el fallo típico de un compose al que le falta un archivo o una variable en algo accionable.
pub(super) fn env_hint(e: anyhow::Error, ctx: &Path) -> anyhow::Error {
    let m = format!("{e:#}");
    let missing = m.split("env file ").nth(1).and_then(|r| r.split(" not found").next()).map(|p| {
        Path::new(p.trim()).strip_prefix(ctx).map(|r| r.display().to_string()).unwrap_or_else(|_| p.trim().to_string())
    });
    if let Some(f) = missing {
        return anyhow!("{m}\nEl compose pide el archivo «{f}», que no viene en el repositorio (casi siempre está en .gitignore). Vuelve a traerlo indicando sus variables: web → paso «Variables»; CLI → --env-file / -e.");
    }
    if m.contains("required variable") || m.contains("variable is not set") {
        return anyhow!("{m}\nEl compose necesita variables que no están definidas. Vuelve a traerlo indicándolas (web → paso «Variables»; CLI → --env-file / -e).");
    }
    e
}

/// Si git falló por falta de credenciales, dice cómo darlas.
pub(super) fn auth_hint(e: anyhow::Error) -> anyhow::Error {
    let m = format!("{e:#}");
    if ["terminal prompts disabled", "could not read Username", "Authentication failed", "Repository not found"].iter().any(|k| m.contains(k)) {
        return anyhow!("{m}\n¿repositorio privado? usa https://USUARIO:TOKEN@host/ruta (o --user/--token, o DOCKYARD_GIT_USER/DOCKYARD_GIT_TOKEN)");
    }
    e
}

pub enum GitOutcome {
    /// Se levantó con Compose como proyecto <nombre>.
    Compose,
    /// Se construyó una imagen: plan listo para revisar y desplegar.
    Image(Box<Plan>),
}

/// Clona (o actualiza) el repositorio en ~/.local/share/dockyard/src/<nombre>. Devuelve la carpeta.
pub async fn git_fetch(src: &GitSource, out: Out<'_>) -> Result<PathBuf> {
    need("git", "Traer desde Git")?;
    src.validate(false)?;
    if !crate::catalog::valid_name(&src.name) {
        bail!("nombre inválido: «{}» (letras, números, - _ .)", src.name);
    }
    let dir = data_dir().join("src").join(&src.name);
    git_sync(src, &dir, out).await?;
    Ok(dir)
}

/// Deja `dir` con la última versión de `src` (clona o hace fetch + reset).
pub async fn git_sync(src: &GitSource, dir: &Path, out: Out<'_>) -> Result<()> {
    need("git", "Traer desde Git")?;
    src.validate(false)?;
    let dir = dir.to_path_buf();
    if dir.join(".git").exists() {
        let cur = src.git(Some(&dir)).args(["remote", "get-url", "origin"]).output().await?;
        let cur = String::from_utf8_lossy(&cur.stdout).trim().to_string();
        if cur != src.url {
            bail!("{} ya contiene otro repositorio ({cur}); usa otro nombre", dir.display());
        }
        out(format!("Actualizando {}…", dir.display()));
        let mut c = src.git(Some(&dir));
        c.args(["fetch", "--depth", "1", "origin", src.git_ref.as_deref().unwrap_or("HEAD")]);
        run_lines(c, "git fetch", out).await.map_err(auth_hint)?;
        let mut c = src.git(Some(&dir));
        c.args(["reset", "--hard", "FETCH_HEAD"]);
        run_lines(c, "git reset", out).await?;
    } else {
        // solo se borra lo que este clon creó: nunca una carpeta que ya existía
        if dir.exists() {
            bail!("{} ya existe y no es un repositorio; usa otro nombre", dir.display());
        }
        std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
        out(format!("Clonando {} en {}…", src.url, dir.display()));
        let mut c = src.git(None);
        c.args(["clone", "--depth", "1"]);
        if let Some(r) = &src.git_ref {
            c.args(["--branch", r]);
        }
        c.arg("--").arg(&src.url).arg(&dir);
        if let Err(e) = run_lines(c, "git clone", out).await {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(auth_hint(e));
        }
    }
    Ok(())
}

/// Construye una imagen desde una carpeta: con `docker build` si existe (BuildKit, .dockerignore),
/// si no, por la API (empaquetando la carpeta sin .git).
pub async fn build_image(engine: &Engine, ctx: &Path, tag: &str, out: Out<'_>) -> Result<()> {
    if !ctx.join("Dockerfile").is_file() {
        bail!("no hay Dockerfile en {}", ctx.display());
    }
    if which("docker").is_some() {
        let mut c = tokio::process::Command::new("docker");
        c.args(["build", "-t", tag, "."]).current_dir(ctx);
        docker_env(&mut c, engine);
        out(format!("$ docker build -t {tag} ."));
        return run_lines(c, "docker build", out).await;
    }
    out(format!("Construyendo {tag} por la API (sin BuildKit)…"));
    let ctx2 = ctx.to_path_buf();
    let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut b = tar::Builder::new(Vec::new());
        b.follow_symlinks(false);
        for e in std::fs::read_dir(&ctx2)?.flatten() {
            let name = e.file_name();
            if name == ".git" {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                b.append_dir_all(&name, &p)?;
            } else {
                b.append_path_with_name(&p, &name)?;
            }
        }
        Ok(b.into_inner()?)
    })
    .await??;
    let opts = BuildImageOptionsBuilder::default().t(tag).rm(true).forcerm(true).build();
    let mut s = engine.d.build_image(opts, None, Some(bollard::body_full(bytes::Bytes::from(data))));
    while let Some(item) = s.next().await {
        let info = item.map_err(|e| anyhow!("{e}"))?;
        if let Some(e) = info.error_detail.and_then(|d| d.message) {
            bail!("la construcción falló: {e}");
        }
        if let Some(l) = info.stream {
            for line in l.lines().filter(|x| !x.trim().is_empty()) {
                out(strip_ansi(line));
            }
        }
    }
    Ok(())
}

/// Puertos «p:p[/udp]» a partir del EXPOSE de una imagen.
pub async fn exposed_ports(engine: &Engine, image: &str) -> Vec<String> {
    let Ok(i) = engine.d.inspect_image(image).await else { return vec![] };
    let mut v: Vec<String> = i
        .config
        .and_then(|c| c.exposed_ports)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|k| {
            let (p, proto) = k.split_once('/').unwrap_or((k.as_str(), "tcp"));
            let p: u16 = p.parse().ok()?;
            Some(if proto == "tcp" { format!("{p}:{p}") } else { format!("{p}:{p}/{proto}") })
        })
        .collect();
    v.sort();
    v
}

/// Trae un repositorio y lo levanta: Compose si trae archivo compose; si no, construye su Dockerfile
/// y devuelve el plan para revisarlo (puertos del EXPOSE, etiquetas de origen para actualizar).
pub async fn git_deploy(engine: &Engine, src: &GitSource, tmpl: &Template, tz: Option<&str>, restart: &str, out: Out<'_>) -> Result<GitOutcome> {
    let dir = git_fetch(src, out).await?;
    let ctx = repo_ctx(&dir, src.subdir.as_deref())?;
    if !ctx.is_dir() {
        bail!("no existe la carpeta {} en el repositorio", ctx.display());
    }
    if let Some(f) = find_compose_file(&ctx) {
        for e in &src.env {
            write_env(&ctx, e)?;
            out(format!("✔ variables guardadas en {} (0600)", e.name));
        }
        compose(engine, &project_name(&src.name), &[f], &["up", "-d", "--build", "--remove-orphans"], out).await.map_err(|e| env_hint(e, &ctx))?;
        return Ok(GitOutcome::Compose);
    }
    if !ctx.join("Dockerfile").is_file() {
        bail!("no encontré compose.yaml ni Dockerfile en {}", ctx.display());
    }
    let tag = format!("dockyard/{}:latest", src.name.to_lowercase());
    build_image(engine, &ctx, &tag, out).await?;
    let mut plan = Plan::build(tmpl, &src.name, tz, restart);
    plan.template_name = format!("Git · {}", src.url);
    plan.services[0].image = tag.clone();
    // un Dockerfile no lee .env del repo: las variables van al contenedor (y no se copian a la imagen)
    for e in &src.env {
        for (k, v) in parse_env(&e.text)? {
            let line = format!("{k}={v}");
            match plan.services[0].env.iter_mut().find(|x| x.split('=').next() == Some(k.as_str())) {
                Some(slot) => *slot = line,
                None => plan.services[0].env.push(line),
            }
        }
    }
    plan.services[0].ports = exposed_ports(engine, &tag).await;
    plan.labels.insert("dockyard.git.url".into(), src.url.clone());
    plan.labels.insert("dockyard.git.dir".into(), dir.display().to_string());
    if let Some(r) = &src.git_ref {
        plan.labels.insert("dockyard.git.ref".into(), r.clone());
    }
    if let Some(s) = src.subdir.as_ref().filter(|s| !s.is_empty()) {
        plan.labels.insert("dockyard.git.subdir".into(), s.clone());
    }
    out(format!("✔ Imagen {tag} lista"));
    Ok(GitOutcome::Image(Box::new(plan)))
}

