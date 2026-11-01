use super::*;
use hyper::Request;

/// Petición real de hyper (sin red) convertida a nuestro formato.
fn req(build: hyper::http::request::Builder) -> Req {
    let (parts, _) = build.body(()).unwrap().into_parts();
    from_parts(&parts)
}

#[test]
fn request_parsing_and_auth() {
    let r = req(Request::builder()
        .method("POST")
        .uri("/api/act?id=ab%20c&a=stop")
        .header("host", "x")
        .header("x-dockyard-token", "secreto")
        .header("content-length", "12"));
    assert_eq!((r.method.as_str(), r.path.as_str(), r.length), ("POST", "/api/act", 12));
    assert_eq!(r.q("id"), Some("ab c"));
    assert_eq!(r.q("a"), Some("stop"));
    assert!(authed(&r, "secreto"));
    assert!(!authed(&r, "secret"));
    assert!(!authed(&req(Request::builder().uri("/").header("x-dockyard-token", "")), "secreto"));
    // una cookie ya no autentica
    assert!(!authed(&req(Request::builder().uri("/").header("cookie", "dytoken=secreto")), "secreto"));
    assert!(req(Request::builder().method("POST").uri("/").header("transfer-encoding", "chunked")).chunked);
    assert_eq!(url_decode("a%2Fb+c%"), "a/b c%");
    assert_eq!(url_decode("%zz"), "%zz");
}

#[test]
fn host_and_origin_must_name_the_listener() {
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    let none: Vec<String> = vec![];
    assert!(host_ok("127.0.0.1:8787", lo, &none));
    assert!(host_ok("localhost:8787", lo, &none));
    assert!(host_ok("LOCALHOST", lo, &none));
    assert!(!host_ok("evil.example:8787", lo, &none));
    assert!(!host_ok("192.168.1.5:8787", lo, &none));
    assert!(!host_ok("", lo, &none));
    // escuchando en «todas», cualquier IP literal sirve, pero nunca un nombre DNS (rebinding)
    let any: IpAddr = "0.0.0.0".parse().unwrap();
    assert!(host_ok("100.64.0.9:8787", any, &none));
    assert!(host_ok("[::1]:8787", "::".parse().unwrap(), &none));
    assert!(!host_ok("miservidor.ts.net:8787", any, &none));
    assert!(origin_ok("http://127.0.0.1:8787", lo, &none));
    assert!(!origin_ok("https://evil.example", lo, &none));
    assert!(!origin_ok("null", lo, &none));
    // --allow-host: solo los nombres que el usuario declara (Caddy, tailscale serve)
    let allow = vec!["panel.midominio.com".to_string()];
    assert!(host_ok("panel.midominio.com", lo, &allow));
    assert!(origin_ok("https://panel.midominio.com", lo, &allow));
    assert!(!host_ok("otro.midominio.com", lo, &allow));
}

#[test]
fn temporary_links_expire() {
    let now = 1_000_000;
    let text = new_session("", "vigente", now + 60, now);
    assert!(session_valid(&text, "vigente", now));
    assert!(!session_valid(&text, "vigente", now + 60)); // vence exactamente ahora
    assert!(!session_valid(&text, "otro", now));
    assert!(!session_valid("", "vigente", now));
    // al crear uno nuevo se podan los vencidos
    let text = new_session(&text, "nuevo", now + 3600, now + 120);
    assert!(!text.contains("vigente"));
    assert!(session_valid(&text, "nuevo", now + 120));
}

#[test]
fn lockout_grows_and_caps() {
    assert_eq!(lockout(FAIL_FREE + 1), Duration::from_secs(30));
    assert_eq!(lockout(FAIL_FREE + 2), Duration::from_secs(60));
    assert_eq!(lockout(50), Duration::from_secs(900));
}

#[test]
fn plan_roundtrips_as_json() {
    let (cat, _) = Catalog::load();
    let plan = Plan::build(cat.get("postgres").unwrap(), "pg", None, "unless-stopped");
    let back: Plan = serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert_eq!(back.services[0].env, plan.services[0].env);
    assert_eq!(back.container_name(0), "pg");
    // el formulario puede mandar campos de menos
    let min: Plan = serde_json::from_str(r#"{"name":"x","services":[{"image":"redis","restart":"no"}]}"#).unwrap();
    assert!(min.errors().is_empty());
}

#[test]
fn embedded_assets_are_complete() {
    // si un include_str! apunta a un archivo vacío o cortado, la binaria sale sin panel: que falle aquí
    assert!(PAGE.contains("<html") && PAGE.trim_end().ends_with("</html>"), "web.html incompleto");
    assert!(CSS.len() > 1000 && CSS.contains('{'), "web.css vacío o no compilado");
}

#[tokio::test]
async fn file_responses_stream_the_whole_file() {
    use http_body_util::BodyExt;
    let p = std::env::temp_dir().join(format!("dy-test-{}", rand_alnum(8)));
    std::fs::write(&p, vec![7u8; 300_000]).unwrap();
    let r = Resp {
        status: "200 OK",
        ctype: "application/octet-stream",
        body: vec![],
        extra: "Content-Disposition: attachment; filename=\"x.tar.gz\"\r\n".into(),
        file: Some(p.clone()),
    };
    let resp = to_hyper(r).await;
    assert_eq!(resp.headers()["content-length"], "300000");
    assert_eq!(resp.headers()["content-disposition"], "attachment; filename=\"x.tar.gz\"");
    assert_eq!(resp.headers()["x-frame-options"], "DENY");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.len(), 300_000);
    std::fs::remove_file(&p).ok();
}
