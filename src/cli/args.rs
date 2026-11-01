//! Definición de los comandos y argumentos de la CLI (clap).

use super::*;

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
    /// Respaldo de una base de datos (contenedor) o de un volumen; con --all, de todo lo que tenga respaldo
    Backup {
        /// Contenedor o volumen (omítelo con --all)
        target: Option<String>,
        /// Cada contenedor en ejecución con volcado conocido (con --volumes, también todos los volúmenes)
        #[arg(long)]
        all: bool,
        /// Con --all: también los volúmenes
        #[arg(long, requires = "all")]
        volumes: bool,
        /// Con --all: conserva solo los N respaldos más nuevos de cada objetivo que respaldó bien (0 = no borra nada)
        #[arg(long, default_value_t = 0, requires = "all")]
        keep: usize,
        /// Con --keep: muestra qué se borraría sin borrar
        #[arg(long, requires = "keep")]
        dry_run: bool,
        /// No pedir confirmación al borrar respaldos viejos
        #[arg(short, long)]
        yes: bool,
    },
    /// Comprueba que un respaldo se puede leer entero (.tar.gz: se descomprime y se recorre)
    Verify {
        /// Ruta al archivo de respaldo
        file: PathBuf,
    },
    /// Imprime una unidad de systemd (o una línea de cron) que respalda todo cada día a la hora indicada
    Schedule {
        /// Hora diaria HH:MM (24 h)
        #[arg(long, default_value = "03:00")]
        at: String,
        /// Conservar los N respaldos más nuevos de cada objetivo (0 = no borrar)
        #[arg(long, default_value_t = 7)]
        keep: usize,
        /// Incluir los volúmenes
        #[arg(long)]
        volumes: bool,
        /// Formato: systemd (timer de usuario) o cron
        #[arg(long, default_value = "systemd")]
        format: String,
    },
    /// Dice qué contenedores tienen una imagen más nueva. Descarga las imágenes, pero no recrea nada
    Outdated,
    /// Avisa cuando un contenedor se detiene o deja de pasar su healthcheck (vigila en bucle)
    Notify {
        /// Tema de ntfy, p. ej. https://ntfy.sh/mi-tema (o variable DOCKYARD_NTFY_URL)
        #[arg(long)]
        ntfy: Option<String>,
        /// Webhook JSON {"text": ...} (o variable DOCKYARD_WEBHOOK_URL). La URL puede llevar un secreto: prefiere la variable
        #[arg(long)]
        webhook: Option<String>,
        /// Segundos entre revisiones
        #[arg(long, default_value_t = 30)]
        every: u64,
        /// Envía un aviso de prueba y sale
        #[arg(long)]
        test: bool,
    },
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
        /// Usuario para repositorios privados por https (o DOCKYARD_GIT_USER)
        #[arg(long)]
        user: Option<String>,
        /// Token para repositorios privados por https (mejor DOCKYARD_GIT_TOKEN: --token queda visible en `ps`)
        #[arg(long)]
        token: Option<String>,
        /// Variable CLAVE=valor. Con compose se guarda en `.env` del repo (que no suele venir en Git); con Dockerfile va al contenedor. Repetible
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Archivo .env local con las variables (se copia a `.env` del repo, modo 0600)
        #[arg(long = "env-file")]
        env_file: Option<PathBuf>,
        /// Puerto host:contenedor (con Dockerfile). Repetible
        #[arg(short = 'p', long = "port")]
        port: Vec<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Túneles públicos (cloudflared sin cuenta, o ngrok) para probar un contenedor en un sandbox
    Tunnel {
        #[command(subcommand)]
        cmd: TunnelCmd,
    },
    /// Diagnóstico del entorno
    Doctor,
    /// Panel web ligero (estado, contenedores, logs, espacio)
    Web {
        /// Dirección donde escuchar, p. ej. la IP de Tailscale: 100.x.y.z:7681
        #[arg(short, long, default_value = "127.0.0.1:7681")]
        listen: String,
        /// Genera un token nuevo: invalida el anterior (si la URL se filtró o un navegador ya no debe entrar)
        #[arg(long)]
        rotate_token: bool,
        /// Nombre de host adicional aceptado, p. ej. el dominio de Caddy o de `tailscale serve` (repetible)
        #[arg(long = "allow-host", value_name = "NOMBRE")]
        allow_host: Vec<String>,
    },
    /// Enlace temporal para el panel (no cambia el token permanente)
    WebLink {
        /// Horas de validez (1 a 720)
        #[arg(long, default_value_t = 12)]
        hours: u64,
        /// La misma dirección con la que corre `dockyard web`
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
pub enum TunnelCmd {
    /// Abre un túnel público hacia un puerto HTTP del contenedor
    Up {
        name: String,
        /// Puerto del contenedor (por defecto, el único que declare)
        #[arg(short, long)]
        port: Option<u16>,
        /// cloudflare (sin cuenta) o ngrok (necesita DOCKYARD_NGROK_AUTHTOKEN)
        #[arg(long, default_value = "cloudflare")]
        provider: String,
        /// Horas hasta cerrarse solo (0 = sin límite)
        #[arg(long, default_value_t = 2)]
        hours: u32,
        /// ngrok: sin usuario y contraseña (por defecto se piden)
        #[arg(long)]
        open: bool,
        #[arg(short, long)]
        yes: bool,
    },
    /// Lista los túneles abiertos
    Ls,
    /// Cierra un túnel (nombre del contenedor destino o del túnel)
    Down { name: String },
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
