//! Pruebas contra un daemon real. Solo corren con DOCKYARD_IT=1 y solo tocan recursos «dytest-it-*».
//!   DOCKYARD_IT=1 cargo test --test integracion -- --test-threads=1
//!
//! Cada prueba hace una sola cosa y registra lo que crea en una `Limpieza`, que lo borra al terminar
//! aunque la prueba falle a mitad.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn enabled() -> bool {
    std::env::var("DOCKYARD_IT").map(|v| v == "1").unwrap_or(false)
}

/// Salir si no se pidieron pruebas de integración.
macro_rules! requiere_daemon {
    () => {
        if !enabled() {
            eprintln!("omitida: define DOCKYARD_IT=1");
            return;
        }
    };
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dytest-it-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Ejecuta dockyard con la carpeta de respaldos y datos aislada.
fn dy(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dockyard"))
        .args(args)
        .env("DOCKYARD_BACKUPS", dir.join("backups"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let o = dy(dir, args);
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "dockyard {args:?} falló:\n{out}");
    out
}

fn docker(args: &[&str]) -> String {
    let o = Command::new("docker").args(args).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn wait_healthy(name: &str) {
    for _ in 0..60 {
        if docker(&["inspect", name, "--format", "{{.State.Health.Status}}"]) == "healthy" {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    panic!("{name} no llegó a healthy");
}

/// Recursos que una prueba creó. Al salir (también si hay pánico) se borran, para no dejar basura en el daemon.
struct Limpieza {
    contenedores: Vec<String>,
    volumenes: Vec<String>,
    redes: Vec<String>,
}

impl Limpieza {
    fn new(contenedores: &[&str], volumenes: &[&str], redes: &[&str]) -> Self {
        let l = Limpieza {
            contenedores: contenedores.iter().map(|s| s.to_string()).collect(),
            volumenes: volumenes.iter().map(|s| s.to_string()).collect(),
            redes: redes.iter().map(|s| s.to_string()).collect(),
        };
        l.borrar(); // por si una corrida anterior dejó algo
        l
    }

    fn borrar(&self) {
        for c in &self.contenedores {
            let _ = Command::new("docker").args(["rm", "-f", "-v", c]).output();
        }
        for n in &self.redes {
            let _ = Command::new("docker").args(["network", "rm", n]).output();
        }
        for v in &self.volumenes {
            let _ = Command::new("docker").args(["volume", "rm", "-f", v]).output();
        }
    }
}

impl Drop for Limpieza {
    fn drop(&mut self) {
        self.borrar();
    }
}

/// Volumen con un archivo de dueño 70:70, como el de PostgreSQL, para comprobar que los dueños sobreviven.
fn volumen_con_archivo(nombre: &str) {
    let _ = docker(&["volume", "create", nombre]);
    let script = "echo hola > /d/archivo && chown 70:70 /d/archivo";
    docker(&["run", "--rm", "-v", &format!("{nombre}:/d"), "busybox:stable", "sh", "-c", script]);
}

fn dueño(volumen: &str) -> String {
    docker(&["run", "--rm", "-v", &format!("{volumen}:/d:ro"), "busybox:stable", "stat", "-c", "%u:%g %a", "/d/archivo"])
}

#[test]
fn postgres_arranque_credenciales_y_actualizacion() {
    requiere_daemon!();
    let d = tmp("pg-cred");
    let c = "dytest-it-pg-cred";
    let _l = Limpieza::new(&[c], &[], &[]);

    let out = ok(&d, &["up", "postgres", "--name", c, "-p", "15500:5432", "-y"]);
    assert!(out.contains("POSTGRES_PASSWORD"), "{out}");
    // rotación de logs y healthcheck aplicados
    let log = docker(&["inspect", c, "--format", "{{json .HostConfig.LogConfig.Config}}"]);
    assert!(log.contains("max-size"), "{log}");
    wait_healthy(c);

    assert!(ok(&d, &["creds", c]).contains("POSTGRES_PASSWORD"));
    ok(&d, &["env", c, "--set", "DYTEST=1", "-y"]);
    assert!(ok(&d, &["env", c]).contains("DYTEST=1"));
    assert!(ok(&d, &["update", c]).contains("al día"));
    ok(&d, &["down", c, "--volumes", "-y"]);
}

#[test]
fn postgres_respaldo_sql_y_restauracion() {
    requiere_daemon!();
    let d = tmp("pg-sql");
    let c = "dytest-it-pg-sql";
    let _l = Limpieza::new(&[c], &[], &[]);

    ok(&d, &["up", "postgres", "--name", c, "-p", "15499:5432", "-y"]);
    wait_healthy(c);
    let sql = |q: &str| ok(&d, &["exec", c, "--", "psql", "-U", "app", "-d", "app", "-Atc", q]);
    sql("create table t(x int); insert into t values (1),(2),(3)");
    ok(&d, &["backup", c]);
    sql("delete from t; insert into t values (99)");
    ok(&d, &["restore", c, "-y"]);
    assert_eq!(sql("select string_agg(x::text, ',' order by x) from t").trim(), "1,2,3");

    // un SQL malo falla y lo dice
    let bad = d.join("malo.sql");
    std::fs::write(&bad, "insert into no_existe values (1);\n").unwrap();
    let o = dy(&d, &["restore", c, bad.to_str().unwrap(), "-y"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("no_existe"));
    ok(&d, &["down", c, "--volumes", "-y"]);
}

#[test]
fn respaldo_y_restauracion_de_volumen_conserva_dueños() {
    requiere_daemon!();
    let d = tmp("vol-resp");
    let _l = Limpieza::new(&[], &["dytest-it-vol-src", "dytest-it-vol-rest"], &[]);
    volumen_con_archivo("dytest-it-vol-src");

    let out = ok(&d, &["backup", "dytest-it-vol-src"]);
    let file = out.split_whitespace().find(|w| w.ends_with(".tar.gz")).unwrap().to_string();
    // restaurar en un volumen que no existe lo crea
    ok(&d, &["restore", "dytest-it-vol-rest", &file, "--clean", "-y"]);
    assert_eq!(dueño("dytest-it-vol-rest"), dueño("dytest-it-vol-src"));
    assert_eq!(dueño("dytest-it-vol-rest"), "70:70 644");
}

#[test]
fn volumen_clonar_explorar_y_listar() {
    requiere_daemon!();
    let d = tmp("vol-clon");
    let _l = Limpieza::new(&[], &["dytest-it-vol-src2", "dytest-it-copia"], &[]);
    volumen_con_archivo("dytest-it-vol-src2");

    ok(&d, &["vol", "clone", "dytest-it-vol-src2", "dytest-it-copia"]);
    // no se copia sobre un volumen existente, ni desde uno que no existe
    assert!(!dy(&d, &["vol", "clone", "dytest-it-vol-src2", "dytest-it-copia"]).status.success());
    let o = dy(&d, &["vol", "clone", "dytest-it-no-existe", "dytest-it-otro"]);
    assert!(!o.status.success());
    assert!(docker(&["volume", "ls", "-q", "--filter", "name=dytest-it-otro"]).is_empty(), "creó un volumen desde uno inexistente");

    assert!(ok(&d, &["vol", "du", "dytest-it-copia"]).contains("Uso total"));
    assert!(ok(&d, &["vol", "ls"]).contains("dytest-it-copia"));
    assert!(ok(&d, &["df"]).contains("Volúmenes"));
    assert!(docker(&["ps", "-a", "--filter", "label=dockyard.helper", "-q"]).is_empty(), "quedaron auxiliares");
}

#[test]
fn editar_archivos_en_contenedor_y_volumen() {
    requiere_daemon!();
    let d = tmp("editar");
    let c = "dytest-it-ed";
    let _l = Limpieza::new(&[c], &["dytest-it-ed-vol"], &[]);
    docker(&["run", "-d", "--name", c, "busybox:stable", "sleep", "600"]);
    docker(&["exec", c, "sh", "-c", "echo base > /tmp/nota.txt"]);
    volumen_con_archivo("dytest-it-ed-vol");

    // editor de prueba que agrega una línea
    let ed = d.join("editor.sh");
    std::fs::write(&ed, "#!/bin/sh\necho '# dytest' >> \"$1\"\n").unwrap();
    std::fs::set_permissions(&ed, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let edit = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_dockyard")).args(args).env("EDITOR", &ed).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    edit(&["edit", c, "/tmp/nota.txt"]);
    assert_eq!(docker(&["exec", c, "cat", "/tmp/nota.txt"]), "base\n# dytest");
    edit(&["vol", "edit", "dytest-it-ed-vol", "archivo"]);
    assert_eq!(docker(&["run", "--rm", "-v", "dytest-it-ed-vol:/d:ro", "busybox:stable", "cat", "/d/archivo"]), "hola\n# dytest");
}

#[test]
fn git_con_dockerfile_y_actualizacion() {
    requiere_daemon!();
    let d = tmp("git");
    let c = "dytest-it-git";
    let _l = Limpieza::new(&[c], &[], &[]);
    let repo = d.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let write_df = |v: &str| {
        std::fs::write(
            repo.join("Dockerfile"),
            format!("FROM busybox:stable\nRUN mkdir /www && echo {v} > /www/index.html\nEXPOSE 8080\nCMD [\"httpd\",\"-f\",\"-p\",\"8080\",\"-h\",\"/www\"]\n"),
        )
        .unwrap();
    };
    let git = |args: &[&str]| {
        assert!(Command::new("git").arg("-C").arg(&repo).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).status().unwrap().success())
    };
    write_df("v1");
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "v1"]);
    let url = format!("file://{}", repo.display());
    ok(&d, &["git", &url, "-n", c, "-p", "18199:8080", "-y"]);
    assert!(docker(&["inspect", c, "--format", "{{index .Config.Labels \"dockyard.git.url\"}}"]).starts_with("file://"));
    assert!(ok(&d, &["update", c]).contains("al día"));
    write_df("v2");
    git(&["commit", "-qam", "v2"]);
    assert!(ok(&d, &["update", c]).contains("actualizado"));
    assert_eq!(docker(&["exec", c, "cat", "/www/index.html"]), "v2");
    ok(&d, &["down", c, "-y"]);
}

#[test]
fn compose_up_ps_y_down() {
    requiere_daemon!();
    if !Command::new("docker").args(["compose", "version"]).output().map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("omitida: no hay `docker compose`");
        return;
    }
    let d = tmp("compose");
    let proyecto = "dytest-it-compose";
    let _l = Limpieza::new(&["dytest-it-compose-web-1"], &[], &["dytest-it-compose_default"]);
    let c = d.join(proyecto);
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("compose.yaml"), "services:\n  web:\n    image: busybox:stable\n    command: [\"sleep\", \"600\"]\n").unwrap();

    ok(&d, &["compose", "up", c.to_str().unwrap()]);
    assert!(ok(&d, &["compose", "ps", proyecto]).contains("dytest-it-compose-web-1"));
    ok(&d, &["down", proyecto, "-y"]);
    assert!(docker(&["network", "ls", "--filter", "name=dytest-it-compose", "-q"]).is_empty());
}

/// `backup --all` respalda TODOS los contenedores en ejecución con volcado conocido, así que solo corre en un daemon
/// aislado (la matriz de versiones lo pone). En tu daemon se omite para no volcar tus bases de datos.
#[test]
fn backup_all_con_retencion() {
    requiere_daemon!();
    if std::env::var("DOCKYARD_IT_AISLADO").map(|v| v != "1").unwrap_or(true) {
        eprintln!("omitida: solo en daemon aislado (DOCKYARD_IT_AISLADO=1)");
        return;
    }
    let d = tmp("all");
    let c = "dytest-it-all-pg";
    let _l = Limpieza::new(&[c], &[], &[]);
    ok(&d, &["up", "postgres", "--name", c, "-p", "15501:5432", "-y"]);
    wait_healthy(c);

    ok(&d, &["backup", "--all", "--keep", "1", "-y"]);
    // dos respaldos en segundos distintos: el segundo debe dejar solo uno
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = ok(&d, &["backup", "--all", "--keep", "1", "-y"]);
    assert!(out.contains("sobran"), "{out}");

    let backups: Vec<PathBuf> = std::fs::read_dir(d.join("backups"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("dytest-it-all-pg-"))
        .collect();
    assert_eq!(backups.len(), 1, "quedaron {backups:?}");
    let verify = ok(&d, &["verify", backups[0].to_str().unwrap()]);
    assert!(verify.contains("no está vacío"), "{verify}");

    ok(&d, &["down", c, "--volumes", "-y"]);
}

/// Si el contenedor recreado no queda sano, el cambio se revierte: vuelve el original, con su nombre y sin el cambio.
#[test]
fn cambio_con_healthcheck_fallido_se_revierte() {
    requiere_daemon!();
    let d = tmp("rollback");
    let c = "dytest-it-hc";
    let _l = Limpieza::new(&[c], &[], &[]);
    docker(&[
        "run", "-d", "--name", c, "--health-cmd", "false", "--health-interval", "1s", "--health-retries", "1",
        "busybox:stable", "sleep", "600",
    ]);
    for _ in 0..30 {
        if docker(&["inspect", c, "--format", "{{.State.Health.Status}}"]) == "unhealthy" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    assert_eq!(docker(&["inspect", c, "--format", "{{.State.Health.Status}}"]), "unhealthy");

    let o = dy(&d, &["env", c, "--set", "DYTEST_CAMBIO=1", "-y"]);
    assert!(!o.status.success(), "el cambio debería fallar");
    assert!(String::from_utf8_lossy(&o.stderr).contains("revertidos"), "{}", String::from_utf8_lossy(&o.stderr));

    assert_eq!(docker(&["inspect", c, "--format", "{{.State.Running}}"]), "true", "el original debe seguir en marcha");
    let env = docker(&["inspect", c, "--format", "{{json .Config.Env}}"]);
    assert!(!env.contains("DYTEST_CAMBIO"), "el original no debe tener el cambio: {env}");
    assert!(docker(&["ps", "-a", "--filter", &format!("name=^{c}-old-"), "-q"]).is_empty(), "quedó un contenedor «-old-»");
}
