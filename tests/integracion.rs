//! Pruebas contra un daemon real. Solo corren con DOCKYARD_IT=1 y solo tocan recursos «dytest-it-*».
//!   DOCKYARD_IT=1 cargo test --test integracion -- --test-threads=1

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn enabled() -> bool {
    std::env::var("DOCKYARD_IT").map(|v| v == "1").unwrap_or(false)
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

#[test]
fn ciclo_completo_postgres() {
    if !enabled() {
        eprintln!("omitida: define DOCKYARD_IT=1");
        return;
    }
    let d = tmp("pg");
    let _ = dy(&d, &["down", "dytest-it-pg", "--volumes", "-y"]);
    let _ = dy(&d, &["vol", "rm", "dytest-it-copia", "dytest-it-rest", "-y"]);

    let out = ok(&d, &["up", "postgres", "--name", "dytest-it-pg", "-p", "15499:5432", "-y"]);
    assert!(out.contains("POSTGRES_PASSWORD"), "{out}");
    // rotación de logs y healthcheck aplicados
    let log = docker(&["inspect", "dytest-it-pg", "--format", "{{json .HostConfig.LogConfig.Config}}"]);
    assert!(log.contains("max-size"), "{log}");
    wait_healthy("dytest-it-pg");

    let sql = |q: &str| ok(&d, &["exec", "dytest-it-pg", "--", "psql", "-U", "app", "-d", "app", "-Atc", q]);
    sql("create table t(x int); insert into t values (1),(2),(3)");
    ok(&d, &["backup", "dytest-it-pg"]);
    sql("delete from t; insert into t values (99)");
    ok(&d, &["restore", "dytest-it-pg", "-y"]);
    assert_eq!(sql("select string_agg(x::text, ',' order by x) from t").trim(), "1,2,3");

    // un SQL malo falla y lo dice
    let bad = d.join("malo.sql");
    std::fs::write(&bad, "insert into no_existe values (1);\n").unwrap();
    let o = dy(&d, &["restore", "dytest-it-pg", bad.to_str().unwrap(), "-y"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("no_existe"));

    // respaldo de volumen y restauración en otro, conservando dueños
    let out = ok(&d, &["backup", "dytest-it-pg-data"]);
    let file = out.split_whitespace().find(|w| w.ends_with(".tar.gz")).unwrap().to_string();
    ok(&d, &["restore", "dytest-it-rest", &file, "--clean", "-y"]);
    let owner = |v: &str| docker(&["run", "--rm", "-v", &format!("{v}:/d:ro"), "alpine:3", "stat", "-c", "%u:%g %a", "/d/PG_VERSION"]);
    assert_eq!(owner("dytest-it-rest"), owner("dytest-it-pg-data"));

    // clonar, explorar, listar
    ok(&d, &["vol", "clone", "dytest-it-pg-data", "dytest-it-copia"]);
    assert!(!dy(&d, &["vol", "clone", "dytest-it-pg-data", "dytest-it-copia"]).status.success());
    assert!(ok(&d, &["vol", "du", "dytest-it-copia"]).contains("Uso total"));
    assert!(ok(&d, &["vol", "ls"]).contains("dytest-it-copia"));
    assert!(ok(&d, &["df"]).contains("Volúmenes"));

    // editar archivos (editor de prueba que agrega una línea)
    let ed = d.join("editor.sh");
    std::fs::write(&ed, "#!/bin/sh\necho '# dytest' >> \"$1\"\n").unwrap();
    std::fs::set_permissions(&ed, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let edit = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_dockyard")).args(args).env("EDITOR", &ed).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    edit(&["edit", "dytest-it-pg", "/var/lib/postgresql/data/postgresql.conf"]);
    let conf = docker(&["exec", "dytest-it-pg", "sh", "-c", "tail -1 /var/lib/postgresql/data/postgresql.conf; stat -c %u /var/lib/postgresql/data/postgresql.conf"]);
    assert_eq!(conf, "# dytest\n70");
    edit(&["vol", "edit", "dytest-it-copia", "nuevo.txt"]);
    assert_eq!(docker(&["run", "--rm", "-v", "dytest-it-copia:/d:ro", "alpine:3", "cat", "/d/nuevo.txt"]), "# dytest");

    // credenciales, variables y actualización
    assert!(ok(&d, &["creds", "dytest-it-pg"]).contains("POSTGRES_PASSWORD"));
    ok(&d, &["env", "dytest-it-pg", "--set", "DYTEST=1", "-y"]);
    assert!(ok(&d, &["env", "dytest-it-pg"]).contains("DYTEST=1"));
    assert!(ok(&d, &["update", "dytest-it-pg"]).contains("al día"));

    ok(&d, &["down", "dytest-it-pg", "--volumes", "-y"]);
    ok(&d, &["vol", "rm", "dytest-it-copia", "dytest-it-rest", "-y"]);
    assert!(docker(&["ps", "-a", "--filter", "label=dockyard.helper", "-q"]).is_empty(), "quedaron auxiliares");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn git_dockerfile_y_compose() {
    if !enabled() {
        return;
    }
    let d = tmp("git");
    let _ = dy(&d, &["down", "dytest-it-git", "-y"]);
    let _ = dy(&d, &["down", "dytest-it-compose", "--volumes", "-y"]);
    let repo = d.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let write_df = |v: &str| {
        std::fs::write(
            repo.join("Dockerfile"),
            format!("FROM busybox:stable\nRUN mkdir /www && echo {v} > /www/index.html\nEXPOSE 8080\nCMD [\"httpd\",\"-f\",\"-p\",\"8080\",\"-h\",\"/www\"]\n"),
        )
        .unwrap();
    };
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).status().unwrap().success());
    write_df("v1");
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "v1"]);
    let url = format!("file://{}", repo.display());
    ok(&d, &["git", &url, "-n", "dytest-it-git", "-p", "18199:8080", "-y"]);
    assert!(docker(&["inspect", "dytest-it-git", "--format", "{{index .Config.Labels \"dockyard.git.url\"}}"]).starts_with("file://"));
    assert!(ok(&d, &["update", "dytest-it-git"]).contains("al día"));
    write_df("v2");
    git(&["commit", "-qam", "v2"]);
    assert!(ok(&d, &["update", "dytest-it-git"]).contains("actualizado"));
    assert_eq!(docker(&["exec", "dytest-it-git", "cat", "/www/index.html"]), "v2");
    ok(&d, &["down", "dytest-it-git", "-y"]);

    // Compose (solo si está instalado)
    if Command::new("docker").args(["compose", "version"]).output().map(|o| o.status.success()).unwrap_or(false) {
        let c = d.join("dytest-it-compose");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("compose.yaml"), "services:\n  web:\n    image: busybox:stable\n    command: [\"sleep\", \"600\"]\n").unwrap();
        ok(&d, &["compose", "up", c.to_str().unwrap()]);
        assert!(ok(&d, &["compose", "ps", "dytest-it-compose"]).contains("dytest-it-compose-web-1"));
        ok(&d, &["down", "dytest-it-compose", "-y"]);
        assert!(docker(&["network", "ls", "--filter", "name=dytest-it-compose", "-q"]).is_empty());
    }
    let _ = std::fs::remove_dir_all(&d);
}
