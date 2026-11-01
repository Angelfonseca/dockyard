
use super::*;

#[test]
fn docker_context_lookup() {
    let d = std::env::temp_dir().join(format!("dy-ctx-{}", rand_alnum(8)));
    let meta = d.join("contexts").join("meta").join("abc123");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(d.join("config.json"), r#"{"currentContext":"remoto"}"#).unwrap();
    std::fs::write(meta.join("meta.json"), r#"{"Name":"remoto","Endpoints":{"docker":{"Host":"ssh://yo@servidor"}}}"#).unwrap();
    assert_eq!(context_host(&d, None), Some(("remoto".into(), "ssh://yo@servidor".into())));
    // DOCKER_CONTEXT manda sobre config.json; «default» = sin contexto
    assert_eq!(context_host(&d, Some("default".into())), None);
    assert_eq!(context_host(&d, Some("otro".into())), None);
    std::fs::write(d.join("config.json"), r#"{"auths":{}}"#).unwrap();
    assert_eq!(context_host(&d, None), None);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn ssh_targets() {
    let mut e = Engine::offline();
    assert_eq!(e.ssh_target(), None);
    e.host = "ssh://yo@srv:2222".into();
    assert_eq!(e.ssh_target(), Some(("yo@srv".into(), Some(2222))));
    e.host = "ssh://srv".into();
    assert_eq!(e.ssh_target(), Some(("srv".into(), None)));
    assert!(!e.is_local());
}

#[test]
fn hints() {
    assert!(missing_binary_hint("OCI runtime exec failed: exec: \"sh\": executable file not found in $PATH").is_some());
    assert!(missing_binary_hint("todo bien").is_none());
    assert!(health_config("").is_none());
    let h = health_config("pg_isready").unwrap();
    assert_eq!(h.test.unwrap()[0], "CMD-SHELL");
}
