//! Pruebas de la CLI.

use super::*;


#[test]
fn repo_names() {
    assert_eq!(repo_name("https://github.com/u/mi-app.git"), "mi-app");
    assert_eq!(repo_name("git@github.com:u/otra.git"), "otra");
    assert_eq!(repo_name("https://x.org/a/b/"), "b");
    assert_eq!(repo_name(""), "app");
}

#[test]
fn git_auth_split() {
    let a = |u: &str, t: &str| Some((u.to_string(), t.to_string()));
    assert_eq!(ext::split_auth("https://u:tok@github.com/o/r.git"), ("https://github.com/o/r.git".into(), a("u", "tok")));
    assert_eq!(ext::split_auth("https://tok@gitlab.com/o/r"), ("https://gitlab.com/o/r".into(), a("git", "tok")));
    assert_eq!(ext::split_auth("https://github.com/o/r"), ("https://github.com/o/r".into(), None));
    assert_eq!(ext::split_auth("git@github.com:o/r.git"), ("git@github.com:o/r.git".into(), None));
    assert_eq!(ext::split_auth("https://u:t@h"), ("https://h".into(), a("u", "t")));
}
