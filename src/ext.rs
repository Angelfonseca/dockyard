//! Programas externos opcionales: docker compose, git, docker build, el editor y comandos del host.
//! Todo lo esencial va por la API; esto solo se usa si el programa existe, y si falta se dice qué instalar.

use crate::catalog::{Plan, Template};
use crate::engine::{Engine, Prog, UpdateOutcome};
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use bollard::query_parameters::*;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, BufReader};

pub type Out<'a> = &'a mut (dyn FnMut(String) + Send);

pub const COMPOSE_FILES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yml", "docker-compose.yaml"];

// ───────────────────────────── Detección ─────────────────────────────

/// Programa para Compose: `docker compose` (v2) o `docker-compose` (v1). None si no hay ninguno.
pub fn compose_cmd() -> Option<Vec<String>> {
    static C: OnceLock<Option<Vec<String>>> = OnceLock::new();
    C.get_or_init(|| {
        if which("docker").is_some() {
            let ok = std::process::Command::new("docker")
                .args(["compose", "version"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Some(vec!["docker".into(), "compose".into()]);
            }
        }
        which("docker-compose").map(|_| vec!["docker-compose".into()])
    })
    .clone()
}

fn need(prog: &str, what: &str) -> Result<()> {
    if which(prog).is_none() {
        bail!("{what} necesita «{prog}», que no está instalado (en Arch: sudo pacman -S {prog})");
    }
    Ok(())
}

/// Editor para archivos: $VISUAL, $EDITOR, o el primero de nano/vim/vi.
pub fn editor() -> Option<Vec<String>> {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            let w = split_words(&v);
            if !w.is_empty() {
                return Some(w);
            }
        }
    }
    ["nano", "vim", "vi"].iter().find(|p| which(p).is_some()).map(|p| vec![p.to_string()])
}

// ───────────────────────────── Ejecución con salida por líneas ─────────────────────────────

/// Prepara un comando que hable con el mismo daemon que dockyard.
fn docker_env(cmd: &mut tokio::process::Command, engine: &Engine) {
    cmd.env("DOCKER_HOST", &engine.host).env_remove("DOCKER_CONTEXT");
}

/// Ejecuta y manda cada línea de stdout/stderr a `out`. Error si termina con código ≠ 0.
pub async fn run_lines(mut cmd: tokio::process::Command, label: &str, out: Out<'_>) -> Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().with_context(|| format!("no se pudo ejecutar {label}"))?;
    let mut so = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("sin stdout"))?).lines();
    let mut se = BufReader::new(child.stderr.take().ok_or_else(|| anyhow!("sin stderr"))?).lines();
    let (mut o_done, mut e_done) = (false, false);
    let mut last_err = Vec::new();
    while !(o_done && e_done) {
        tokio::select! {
            l = so.next_line(), if !o_done => match l {
                Ok(Some(l)) => out(strip_ansi(&l)),
                _ => o_done = true,
            },
            l = se.next_line(), if !e_done => match l {
                Ok(Some(l)) => {
                    let l = strip_ansi(&l);
                    last_err.push(l.clone());
                    if last_err.len() > 6 {
                        last_err.remove(0);
                    }
                    out(l);
                }
                _ => e_done = true,
            },
        }
    }
    let st = child.wait().await?;
    if !st.success() {
        bail!("{label} terminó con código {}: {}", st.code().unwrap_or(-1), last_err.join(" · "));
    }
    Ok(())
}

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

// ───────────────────────────── Git ─────────────────────────────

pub struct GitSource {
    pub url: String,
    pub git_ref: Option<String>,
    pub name: String,
    /// Carpeta dentro del repositorio con el compose/Dockerfile.
    pub subdir: Option<String>,
    /// (usuario, token) para repositorios privados por https. Nunca se guarda en etiquetas ni en .git/config.
    pub auth: Option<(String, String)>,
}

impl GitSource {
    /// Arma el origen. `url` puede traer «usuario:token@» (se separa y la URL queda limpia); `user`/`token`
    /// explícitos, o DOCKYARD_GIT_USER / DOCKYARD_GIT_TOKEN, tienen prioridad sobre lo que traiga la URL.
    pub fn new(url: &str, git_ref: Option<String>, name: String, subdir: Option<String>, user: Option<String>, token: Option<String>) -> Self {
        let (url, from_url) = split_auth(url);
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let token = token.or_else(|| env("DOCKYARD_GIT_TOKEN"));
        let user = user.or_else(|| env("DOCKYARD_GIT_USER"));
        let auth = match (user, token, from_url) {
            (u, Some(t), _) => Some((u.unwrap_or_else(|| "git".into()), t)),
            (_, None, a) => a,
        };
        GitSource { url, git_ref, name, subdir, auth }
    }

    fn git(&self, dir: Option<&Path>) -> tokio::process::Command {
        let mut c = tokio::process::Command::new("git");
        if let Some(d) = dir {
            c.arg("-C").arg(d);
        }
        // nunca quedarse esperando usuario/contraseña en la TUI
        c.env("GIT_TERMINAL_PROMPT", "0");
        // solo protocolos de repositorio: sin `ext::` (ejecuta comandos) ni redirecciones a otro host (filtrarían credenciales)
        c.env("GIT_ALLOW_PROTOCOL", "file:https:http:ssh:git");
        c.args(["-c", "http.followRedirects=false"]);
        // Credenciales solo para https: se vacían los helpers del sistema y se usa un archivo propio (0600, lo crea
        // git) donde queda el token tras el primer éxito, así `u` (git pull) funciona sin volver a pedirlo.
        // Con token nuevo, un helper en línea (va primero, para pisar uno viejo) lo entrega por entorno, nunca en argv ni en la URL.
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(data_dir());
            // si ya existía (más abierta), se cierra: aquí vive el archivo de credenciales
            let _ = std::fs::set_permissions(data_dir(), std::fs::Permissions::from_mode(0o700));
        }
        c.args(["-c", "credential.helper="]);
        if let Some((u, t)) = &self.auth {
            // solo responde a `get` y solo para el host de la URL (git manda `host=` por stdin)
            c.args(["-c", "credential.helper=!f() { [ \"$1\" = get ] || return 0; h=; while IFS== read -r k v; do [ \"$k\" = host ] && h=$v; done; \
                [ \"$h\" = \"$DOCKYARD_GIT_HOST\" ] && printf 'username=%s\\npassword=%s\\n' \"$DOCKYARD_GIT_USER\" \"$DOCKYARD_GIT_TOKEN\"; }; f"]);
            c.env("DOCKYARD_GIT_USER", u).env("DOCKYARD_GIT_TOKEN", t).env("DOCKYARD_GIT_HOST", url_host(&self.url));
        }
        c.arg("-c").arg(format!("credential.helper=store --file={}", data_dir().join("git-credentials").display()));
        c
    }
}

/// Para mostrar en pantalla: «https://usuario:token@host/x» → «https://•••••••••••@host/x» (misma longitud).
pub fn mask_auth(url: &str) -> String {
    if split_auth(url).1.is_none() {
        return url.to_string();
    }
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let at = rest.split('/').next().and_then(|a| a.rfind('@')).unwrap_or(0);
    format!("{scheme}://{}{}", "•".repeat(rest[..at].chars().count()), &rest[at..])
}

/// «host[:puerto]» de una URL http(s); vacío si no la tiene.
fn url_host(url: &str) -> String {
    url.split_once("://").map(|(_, r)| r.split('/').next().unwrap_or("").to_string()).unwrap_or_default()
}

impl GitSource {
    /// Rechaza lo que no es un repositorio remoto razonable. `remote_only` (panel web): sin rutas locales ni `file://`.
    pub fn validate(&self, remote_only: bool) -> Result<()> {
        let u = &self.url;
        if u.is_empty() || u.starts_with('-') || u.chars().any(|c| c.is_control() || c.is_whitespace()) {
            bail!("URL de repositorio inválida");
        }
        let scheme = u.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
        let scp_like = scheme.is_none() && u.contains('@') && u.contains(':') && !u.starts_with('/');
        if remote_only && !(scp_like || matches!(scheme.as_deref(), Some("https" | "http" | "ssh" | "git"))) {
            bail!("usa una URL https://…, ssh://… o git@host:ruta");
        }
        if let Some(s) = &self.subdir {
            let p = Path::new(s);
            if p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                bail!("la subcarpeta debe ser relativa y sin «..»");
            }
        }
        // un token por http viaja en claro (se tolera solo hacia esta misma máquina)
        if self.auth.is_some() && scheme.as_deref() == Some("http") {
            let host = url_host(u);
            let host = host.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map_or(host.as_str(), |(h, _)| h);
            if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
                bail!("el token viajaría sin cifrar por http; usa https://");
            }
        }
        Ok(())
    }
}

/// Carpeta de trabajo dentro del repositorio; nunca fuera de él (ni por `..` ni por symlinks).
fn repo_ctx(dir: &Path, subdir: Option<&str>) -> Result<PathBuf> {
    let Some(s) = subdir.filter(|s| !s.is_empty()) else { return Ok(dir.to_path_buf()) };
    let ctx = dir.join(s);
    let (real, root) = (ctx.canonicalize().map_err(|_| anyhow!("no existe la carpeta {} en el repositorio", ctx.display()))?, dir.canonicalize()?);
    if !real.starts_with(&root) {
        bail!("la subcarpeta «{s}» sale del repositorio");
    }
    Ok(real)
}

/// «https://usuario:token@host/x» → («https://host/x», Some((usuario, token))). Un solo valor se toma como token.
pub fn split_auth(url: &str) -> (String, Option<(String, String)>) {
    let Some((scheme, rest)) = url.split_once("://").filter(|(s, _)| *s == "http" || *s == "https") else {
        return (url.to_string(), None);
    };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let Some((info, host)) = authority.rsplit_once('@') else { return (url.to_string(), None) };
    let auth = match info.split_once(':') {
        Some((u, t)) => (u.to_string(), t.to_string()),
        None => ("git".to_string(), info.to_string()),
    };
    let clean = if path.is_empty() { format!("{scheme}://{host}") } else { format!("{scheme}://{host}/{path}") };
    (clean, Some(auth))
}

/// Si git falló por falta de credenciales, dice cómo darlas.
fn auth_hint(e: anyhow::Error) -> anyhow::Error {
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
        compose(engine, &project_name(&src.name), &[f], &["up", "-d", "--build", "--remove-orphans"], out).await?;
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

// ───────────────────────────── Host y editor ─────────────────────────────

/// Shell interactiva de «la máquina»: la local, o la del servidor si el daemon es remoto por SSH.
pub fn host_shell(engine: &Engine) -> std::process::Command {
    if let Some((target, port)) = engine.ssh_target() {
        let mut c = std::process::Command::new("ssh");
        c.arg("-t");
        if let Some(p) = port {
            c.arg("-p").arg(p.to_string());
        }
        c.arg(target);
        return c;
    }
    let sh = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let mut c = std::process::Command::new(sh);
    c.env("DOCKER_HOST", &engine.host);
    c
}

/// Comando de una sola vez en la máquina (local o el servidor SSH).
pub fn host_command(engine: &Engine, line: &str) -> tokio::process::Command {
    if let Some((target, port)) = engine.ssh_target() {
        let mut c = tokio::process::Command::new("ssh");
        c.args(["-o", "BatchMode=yes"]);
        if let Some(p) = port {
            c.arg("-p").arg(p.to_string());
        }
        c.arg(target).arg("--").arg(line);
        return c;
    }
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(line).env("DOCKER_HOST", &engine.host);
    c
}

/// Abre `content` en el editor (bloquea hasta cerrarlo). Some(nuevo) si cambió.
pub fn edit_in_editor(content: &[u8], file_name: &str) -> Result<Option<Vec<u8>>> {
    let Some(ed) = editor() else { bail!("no hay editor: define $EDITOR o instala nano") };
    let safe: String = file_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
    let tmp = std::env::temp_dir().join(format!("dockyard-{}-{safe}", rand_alnum(6)));
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(content)?;
    }
    let st = std::process::Command::new(&ed[0]).args(&ed[1..]).arg(&tmp).status();
    let new = std::fs::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let st = st.with_context(|| format!("no se pudo abrir el editor «{}»", ed[0]))?;
    if !st.success() {
        bail!("el editor terminó con error; no se guardó nada");
    }
    let new = new?;
    Ok(if new == content { None } else { Some(new) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names() {
        assert_eq!(project_name("Mi App"), "mi-app");
        assert_eq!(project_name("--x--"), "x");
        assert_eq!(project_name("..."), "app");
    }

    #[test]
    fn compose_labels() {
        let mut l = HashMap::new();
        l.insert("com.docker.compose.project".to_string(), "web".to_string());
        l.insert("com.docker.compose.service".to_string(), "db".to_string());
        l.insert("com.docker.compose.project.config_files".to_string(), "/a/compose.yaml,/a/override.yaml".to_string());
        let (p, s, f) = compose_origin(&l).unwrap();
        assert_eq!((p.as_str(), s.as_str(), f.len()), ("web", "db", 2));
        assert!(compose_origin(&HashMap::new()).is_none());
    }

    #[test]
    fn finds_compose_file() {
        let d = std::env::temp_dir().join(format!("dy-cf-{}", rand_alnum(8)));
        std::fs::create_dir_all(&d).unwrap();
        assert!(find_compose_file(&d).is_none());
        std::fs::write(d.join("docker-compose.yml"), "services: {}").unwrap();
        assert_eq!(find_compose_file(&d).unwrap(), d.join("docker-compose.yml"));
        std::fs::write(d.join("compose.yaml"), "services: {}").unwrap();
        assert_eq!(find_compose_file(&d).unwrap(), d.join("compose.yaml"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    fn src(url: &str, subdir: Option<&str>, auth: bool) -> GitSource {
        GitSource { url: url.into(), git_ref: None, name: "t".into(), subdir: subdir.map(Into::into), auth: auth.then(|| ("u".into(), "tok".into())) }
    }

    #[test]
    fn git_urls_are_validated() {
        for bad in ["", "--upload-pack=x", "-c", "https://h/r --x", "https://h/r\nhost=evil", "ext::sh -c id"] {
            // `ext::` solo pasa si no es remoto-solo; git lo bloquea aparte (GIT_ALLOW_PROTOCOL)
            if bad.starts_with("ext") {
                assert!(src(bad, None, false).validate(true).is_err(), "{bad}");
            } else {
                assert!(src(bad, None, false).validate(false).is_err(), "{bad:?}");
            }
        }
        for ok in ["https://github.com/o/r.git", "git@github.com:o/r.git", "ssh://git@h/o/r", "git://h/r"] {
            assert!(src(ok, None, false).validate(true).is_ok(), "{ok}");
        }
        // el panel web no acepta rutas locales ni file://
        for local in ["/etc", "file:///etc", "../x", "./r"] {
            assert!(src(local, None, false).validate(true).is_err(), "{local}");
        }
        assert!(src("/tmp/repo", None, false).validate(false).is_ok()); // CLI/TUI sí
        // subcarpetas que se salen del repositorio
        for sub in ["../x", "/etc", "a/../../b"] {
            assert!(src("https://h/r", Some(sub), false).validate(true).is_err(), "{sub}");
        }
        assert!(src("https://h/r", Some("apps/web"), false).validate(true).is_ok());
        // un token nunca viaja por http, salvo hacia esta misma máquina
        assert!(src("http://ejemplo.com/r", None, true).validate(true).is_err());
        assert!(src("http://127.0.0.1:8765/r", None, true).validate(true).is_ok());
        assert!(src("http://ejemplo.com/r", None, false).validate(true).is_ok());
    }

    #[test]
    fn repo_ctx_stays_inside() {
        let d = std::env::temp_dir().join(format!("dy-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("repo/app")).unwrap();
        std::os::unix::fs::symlink("/etc", d.join("repo/fuga")).unwrap();
        let repo = d.join("repo");
        assert!(repo_ctx(&repo, Some("app")).unwrap().ends_with("repo/app"));
        assert_eq!(repo_ctx(&repo, None).unwrap(), repo);
        assert!(repo_ctx(&repo, Some("fuga")).is_err(), "un symlink no debe sacar del repositorio");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn credentials_are_masked() {
        assert_eq!(mask_auth("https://u:tok@github.com/o/r"), "https://•••••@github.com/o/r");
        assert_eq!(mask_auth("https://github.com/o/r"), "https://github.com/o/r");
        assert_eq!(mask_auth("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(mask_auth("https://u:tok@h").chars().count(), "https://u:tok@h".chars().count());
    }

    /// El helper de credenciales solo entrega el token al host de la URL (no a uno al que lo redirijan).
    #[tokio::test]
    async fn token_only_goes_to_its_host() {
        if which("git").is_none() {
            return;
        }
        let fill = |host: &'static str| async move {
            use tokio::io::AsyncWriteExt;
            let mut c = src("https://github.com/o/r", None, true).git(None);
            c.args(["credential", "fill"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            let mut ch = c.spawn().unwrap();
            let mut i = ch.stdin.take().unwrap();
            i.write_all(format!("protocol=https\nhost={host}\n\n").as_bytes()).await.unwrap();
            drop(i);
            String::from_utf8_lossy(&ch.wait_with_output().await.unwrap().stdout).into_owned()
        };
        assert!(fill("github.com").await.contains("password=tok"));
        assert!(!fill("evil.example").await.contains("tok"));
    }
}
