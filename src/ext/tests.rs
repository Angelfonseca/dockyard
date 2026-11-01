//! Pruebas de las utilidades de ext.

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
    GitSource { url: url.into(), git_ref: None, name: "t".into(), subdir: subdir.map(Into::into), auth: auth.then(|| ("u".into(), "tok".into())), env: vec![] }
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

#[test]
fn detects_env_files_in_compose() {
    let y = "services:\n  web:\n    image: x\n    env_file: .env\n  api:\n    env_file:\n      - ./api/.env\n      - path: secrets.env\n        required: false\n    environment:\n      A: 1\n  db:\n    env_file: [db.env, \"extra.env\"]\n  x:\n    env_file: ${CFG}/x.env\n";
    assert_eq!(compose_env_files(y), vec![".env", "api/.env", "db.env", "extra.env", "secrets.env"]);
    assert!(compose_env_files("services: {}").is_empty());
    assert!(compose_env_files("env_file: /etc/passwd\nenv_file: ../x.env").is_empty());
}

#[test]
fn env_text_is_parsed_and_validated() {
    let p = parse_env("# c\nA=1\nexport B=\"dos palabras\"\n\nC='x'\nD=a=b\n").unwrap();
    assert_eq!(p, vec![("A".into(), "1".into()), ("B".into(), "dos palabras".into()), ("C".into(), "x".into()), ("D".into(), "a=b".into())]);
    let e = parse_env("OK=1\nesto no es una variable secreto123").unwrap_err().to_string();
    assert!(e.contains("línea 2") && !e.contains("secreto123"), "{e}"); // el error no repite el contenido
    assert!(parse_env("1A=x").is_err() && parse_env("A B=x").is_err() && parse_env("A=\0").is_err());
    assert!(parse_env(&"A=1\n".repeat(20_000)).is_err());
}

#[test]
fn env_files_never_escape_the_repo() {
    let d = std::env::temp_dir().join(format!("dy-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("repo")).unwrap();
    std::fs::write(d.join("fuera"), "intacto").unwrap();
    std::os::unix::fs::symlink(d.join("fuera"), d.join("repo/.env")).unwrap();       // .env → archivo fuera
    std::os::unix::fs::symlink("/tmp", d.join("repo/enlace")).unwrap();              // carpeta → fuera
    let repo = d.join("repo");
    let f = |n: &str| EnvFile { name: n.into(), text: "A=1\n".into() };
    assert!(write_env(&repo, &f(".env")).is_err(), "no se escribe a través de un symlink");
    assert!(write_env(&repo, &f("enlace/x.env")).is_err());
    assert!(write_env(&repo, &f("../x.env")).is_err() && write_env(&repo, &f("/tmp/x.env")).is_err() && write_env(&repo, &f("")).is_err());
    assert_eq!(std::fs::read_to_string(d.join("fuera")).unwrap(), "intacto");
    write_env(&repo, &f("api/.env")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(repo.join("api/.env")).unwrap().permissions().mode() & 0o777, 0o600);
    std::fs::remove_dir_all(&d).unwrap();
}
