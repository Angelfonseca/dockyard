//! Funciones auxiliares sin estado: nombres sugeridos, rutas de shell y puertos libres.

use super::*;

// ───────────────────────────── Auxiliares ─────────────────────────────

pub fn filtered_len(lv: &LogView) -> usize {
    if lv.filter.is_empty() {
        lv.lines.len()
    } else {
        let f = lv.filter.to_lowercase();
        lv.lines.iter().filter(|l| l.to_lowercase().contains(&f)).count()
    }
}

/// Expande «~» al inicio de una ruta escrita a mano.
pub fn shell_path(p: &str) -> String {
    let p = p.trim();
    if p == "~" {
        home_dir().display().to_string()
    } else if let Some(r) = p.strip_prefix("~/") {
        home_dir().join(r).display().to_string()
    } else {
        p.to_string()
    }
}

pub fn suggest_name(existing: &[CRow], base: &str) -> String {
    let base: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    let taken = |n: &str| existing.iter().any(|c| c.name == n || c.name.starts_with(&format!("{n}-")));
    if !taken(&base) {
        return base;
    }
    for i in 2..100 {
        let n = format!("{base}-{i}");
        if !taken(&n) {
            return n;
        }
    }
    base
}

/// ¿Se puede enlazar el puerto localmente? (permiso denegado cuenta como «no sé» = libre)
pub fn can_bind(port: u16, proto: &str) -> bool {
    use std::io::ErrorKind;
    let r = if proto == "udp" {
        std::net::UdpSocket::bind(("0.0.0.0", port)).map(|_| ())
    } else {
        std::net::TcpListener::bind(("0.0.0.0", port)).map(|_| ())
    };
    match r {
        Ok(()) => true,
        Err(e) => e.kind() != ErrorKind::AddrInUse,
    }
}

pub fn help_lines() -> Vec<String> {
    let t = |s: &str| s.to_string();
    vec![
        t("  dockyard — gestor de Docker en terminal"),
        t(""),
        t("  GLOBAL"),
        t("    1-6 / Tab / [ ]    cambiar de pestaña"),
        t("    X                  shell de la máquina (o del servidor si Docker es remoto por SSH)"),
        t("    !                  comando en la máquina, con salida en vivo"),
        t("    F5                 actualizar ahora"),
        t("    ?                  esta ayuda        q  salir"),
        t(""),
        t("  CONTENEDORES"),
        t("    ↑↓ jk  PgUp/PgDn   moverse            g/G  inicio/fin"),
        t("    Enter / l          ver logs en vivo"),
        t("    s                  iniciar / detener"),
        t("    r                  reiniciar          p  pausar/reanudar"),
        t("    e                  editar (variables, puertos, volúmenes, imagen, RAM, CPU…) y recrear"),
        t("    u                  actualizar (imagen nueva, git pull + build, o compose pull)"),
        t("    x                  shell interactiva (bash/sh) — no necesita el CLI de docker"),
        t("    c                  ejecutar un comando con salida en vivo (↑↓ historial)"),
        t("    F                  editar un archivo con tu $EDITOR (también si está detenido)"),
        t("    i                  inspeccionar (JSON completo)"),
        t("    b                  respaldo de base de datos (pg_dump, mariadb-dump…)"),
        t("    R                  restaurar un respaldo de base de datos"),
        t("    C                  ver contraseñas y tokens del contenedor o su stack"),
        t("    d                  eliminar            D  eliminar todo el stack (también de Compose)"),
        t("    K                  matar (SIGKILL)"),
        t("    a                  mostrar todos / solo activos"),
        t("    o                  cambiar orden       /  filtrar    Esc  limpiar filtro"),
        t("    n                  ir al catálogo para instalar algo nuevo"),
        t(""),
        t("  LOGS Y SALIDA DE COMANDOS"),
        t("    f / Espacio        seguir / pausar     /  filtrar"),
        t("    ↑↓ PgUp PgDn g G   desplazarse         ←→  desplazamiento horizontal"),
        t("    w                  ajustar líneas      t  marcas de tiempo (solo logs)"),
        t("    c                  limpiar pantalla    Esc  cerrar (detiene el comando si sigue)"),
        t(""),
        t("  IMÁGENES"),
        t("    p                  descargar imagen    Enter  crear contenedor desde ella"),
        t("    d                  eliminar            D  limpiar las que nadie usa"),
        t(""),
        t("  VOLÚMENES"),
        t("    Enter              explorar: lo más pesado y su contenido"),
        t("    f                  editar un archivo del volumen"),
        t("    b                  respaldar a .tar.gz  R  restaurar (s sobrescribe, l vacía antes)"),
        t("    c                  clonar (copia exacta)"),
        t("    d                  eliminar             D  limpiar huérfanos"),
        t(""),
        t("  CATÁLOGO"),
        t("    Enter              instalar plantilla (bases de datos, apps, stacks…)"),
        t("    r                  traer desde un repositorio Git (URL[#rama]): compose.yaml o Dockerfile"),
        t("    o                  levantar un proyecto Compose (carpeta o archivo)"),
        t("    /                  buscar"),
        t(""),
        t("  ESPACIO"),
        t("    c i b n v          limpiar contenedores detenidos, imágenes sin uso, caché de build,"),
        t("                       redes sin uso o volúmenes huérfanos (siempre pide confirmación)"),
        t("    r                  recalcular"),
        t(""),
        t("  FORMULARIO"),
        t("    ↑↓                 moverse             Enter  editar / alternar"),
        t("    a                  añadir variable     d  borrar fila"),
        t("    F5 / D             desplegar / aplicar Esc  cancelar"),
        t(""),
        t("  Plantillas propias: ~/.config/dockyard/templates.toml"),
        t("  Ajustes: ~/.config/dockyard/config.toml (tz, backups_dir, refresh_secs, default_restart, log_max_size…)"),
    ]
}
