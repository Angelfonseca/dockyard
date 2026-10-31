//! Repositorios Git: origen, credenciales, clonado, actualización y despliegue desde el repo.

use super::*;

// ───────────────────────────── Git ─────────────────────────────

pub struct GitSource {
    pub url: String,
    pub git_ref: Option<String>,
    pub name: String,
    /// Carpeta dentro del repositorio con el compose/Dockerfile.
    pub subdir: Option<String>,
    /// (usuario, token) para repositorios privados por https. Nunca se guarda en etiquetas ni en .git/config.
    pub auth: Option<(String, String)>,
    /// Archivos de variables que el repo no trae (`.env` suele estar en .gitignore): se escriben antes de levantar.
    pub env: Vec<EnvFile>,
}

/// Un archivo de variables (formato .env) y su ruta relativa a la carpeta del compose.
#[derive(Clone, Debug)]
pub struct EnvFile {
    pub name: String,
    pub text: String,
}

impl EnvFile {
    /// Nombre relativo y sin «..», y contenido con formato NOMBRE=valor. Se comprueba al recibirlo y otra vez al escribir.
    pub fn check(&self) -> Result<()> {
        let rel = Path::new(&self.name);
        if self.name.is_empty() || rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            bail!("nombre de archivo de variables inválido: «{}» (ruta relativa, sin «..»)", self.name);
        }
        parse_env(&self.text).map(|_| ())
    }
}

/// Resultado de mirar un repositorio antes de desplegarlo.
pub struct GitInspect {
    /// «compose» o «dockerfile».
    pub kind: &'static str,
    /// Archivos de variables que el compose pide y no existen: (ruta, contenido de su ejemplo si el repo trae uno).
    pub env_files: Vec<(String, Option<String>)>,
    /// Plantilla genérica del repo (.env.example…), por si no se detectó ningún archivo concreto.
    pub env_example: Option<String>,
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
        GitSource { url, git_ref, name, subdir, auth, env: Vec::new() }
    }

    pub(super) fn git(&self, dir: Option<&Path>) -> tokio::process::Command {
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
pub(super) fn url_host(url: &str) -> String {
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
pub(super) fn repo_ctx(dir: &Path, subdir: Option<&str>) -> Result<PathBuf> {
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

