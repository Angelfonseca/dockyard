//! Pruebas de dibujo: todas las pestañas en varios tamaños de terminal.

use super::*;

use crate::app::{Confirm, ConfirmAction, Mode, Prompt, PromptAction, Tab, TextView};
use crate::catalog::{Catalog, Plan};
use crate::collector::Shared;
use crate::config::Config;
use crate::engine::Engine;
use crate::forms::{Form, FormKind};
use crate::input::LineInput;
use crate::model::{CRow, CStat, DiskInfo, Health, Host, IRow, Snapshot, St, VRow};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::Arc;

fn crow(i: usize, state: St) -> CRow {
    CRow {
        id: format!("{i:064x}"),
        name: format!("servicio-{i}"),
        image: "postgres:17-alpine".into(),
        image_id: "sha256:abc".into(),
        state,
        status: "Up 3 hours".into(),
        health: if i % 2 == 0 { Some(Health::Healthy) } else { None },
        ports: "0.0.0.0:5432->5432/tcp".into(),
        published: vec![(5432 + i as u16, "tcp".into())],
        stack: if i % 3 == 0 { Some("nc".into()) } else { None },
        created: 1_700_000_000,
        mounts: vec!["pg-data".into()],
        networks: vec!["bridge".into()],
        tunnel: None,
    }
}

fn app() -> App {
    let engine = Arc::new(Engine::offline());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (cfg, _) = Config::load();
    let (cat, _) = Catalog::load();
    let shared = Shared::new(cfg.refresh_ms());
    let mut a = App::new(engine, tx, shared, cfg, cat);
    let mut snap = Snapshot::default();
    for i in 0..12 {
        let st = if i % 4 == 3 { St::Exited } else { St::Running };
        let c = crow(i, st);
        if st == St::Running {
            snap.stats.insert(
                c.id.clone(),
                CStat { cpu: 3.0 * i as f64, mem: 80 << 20, mem_limit: 1 << 30, rx: 1 << 20, tx: 1 << 19, rx_rate: 1e5, tx_rate: 2e4 },
            );
        }
        snap.containers.push(c);
    }
    snap.host = Host {
        hostname: "fserver".into(),
        os: "Arch Linux".into(),
        cpu: 37.0,
        cores: vec![10.0, 80.0, 5.0, 33.0, 12.0, 90.0, 1.0, 50.0],
        cpu_model: "AMD Ryzen 7 2700U".into(),
        temp_c: Some(54.0),
        mem_used: 3 << 30,
        mem_total: 8 << 30,
        swap_used: 0,
        swap_total: 4 << 30,
        load: (0.5, 0.7, 0.9),
        uptime: 90_000,
        disks: vec![DiskInfo { mount: "/".into(), used: 40 << 30, total: 230 << 30 }],
        net_rx_rate: 1e6,
        net_tx_rate: 3e5,
    };
    snap.docker_version = "27.0".into();
    snap.images = Some(vec![IRow { id: "sha256:abc".into(), tags: vec!["postgres:17-alpine".into()], size: 250 << 20, created: 1_700_000_000, used_by: vec!["servicio-0".into()] }]);
    snap.volumes = Some(vec![VRow { name: "pg-data".into(), driver: "local".into(), mountpoint: "/var/lib/docker/volumes/pg-data/_data".into(), used_by: vec!["servicio-0".into()] }]);
    a.connected = true;
    a.apply_snapshot(snap);
    a
}

fn render(a: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw(f, a)).unwrap();
    let buf = t.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

#[tokio::test]
async fn all_tabs_all_sizes() {
    let mut a = app();
    for tab in Tab::ALL {
        a.tab = tab;
        for (w, h) in [(160, 50), (120, 40), (80, 24), (50, 14), (20, 6), (1, 1)] {
            let s = render(&mut a, w, h);
            assert_eq!(s.lines().count(), h as usize);
        }
    }
    a.tab = Tab::Containers;
    let s = render(&mut a, 120, 40);
    assert!(s.contains("servicio-0"), "{s}");
    a.tab = Tab::Panel;
    let s = render(&mut a, 120, 40);
    assert!(s.contains("fserver") || s.contains("Ryzen"), "{s}");
    a.tab = Tab::Catalog;
    let s = render(&mut a, 120, 40);
    assert!(s.to_lowercase().contains("postgres"), "{s}");
}

#[tokio::test]
async fn overlays_render() {
    let mut a = app();
    let (cat, _) = Catalog::load();
    let plan = Plan::build(cat.get("postgres").unwrap(), "pg", None, "unless-stopped");
    let mut form = Form::new(FormKind::Deploy, plan);
    form.port_issues.push("5432".into());
    a.mode = Mode::Form(Box::new(form));
    for (w, h) in [(120, 40), (80, 24), (40, 10)] {
        let s = render(&mut a, w, h);
        if w >= 80 {
            assert!(s.contains("POSTGRES_PASSWORD"), "{s}");
        }
    }
    a.mode = Mode::Text(Box::new(TextView { title: "inspect".into(), lines: (0..200).map(|i| format!("línea {i} áéí")).collect(), scroll: 150, hscroll: 3 }));
    render(&mut a, 100, 30);
    render(&mut a, 30, 8);
    a.mode = Mode::Prompt(Box::new(Prompt { title: "Descargar".into(), hint: "imagen".into(), input: LineInput::new("redis:7"), action: PromptAction::Pull }));
    let s = render(&mut a, 100, 30);
    assert!(s.contains("redis:7"), "{s}");
    a.mode = Mode::Confirm(Box::new(Confirm { title: "Eliminar".into(), lines: vec!["¿Seguro?".into()], action: ConfirmAction::PruneImages, danger: true, alt: None }));
    let s = render(&mut a, 100, 30);
    assert!(s.contains("Seguro"), "{s}");
    render(&mut a, 20, 5);
    a.mode = Mode::Confirm(Box::new(Confirm {
        title: "Restaurar".into(),
        lines: vec!["volumen".into()],
        action: ConfirmAction::PruneNetworks,
        danger: true,
        alt: Some(('l', "Vaciar y restaurar".into())),
    }));
    let s = render(&mut a, 100, 30);
    assert!(s.contains("Vaciar y restaurar"), "{s}");
}

#[tokio::test]
async fn space_tab_and_task_view() {
    use crate::model::{DiskUsage, Usage};
    let mut a = app();
    a.tab = Tab::Space;
    let s = render(&mut a, 120, 40);
    assert!(s.contains("Calculando"), "{s}");
    let us = |n: u64| Usage { count: n, active: 1, size: n << 30, reclaimable: n << 29, known: true };
    let mut u = DiskUsage { images: us(4), containers: us(2), volumes: us(3), build: Usage::default(), ..Default::default() };
    u.vol_sizes.insert("pg-data".into(), 5 << 30);
    a.usage = Some(u);
    for (w, h) in [(160, 50), (120, 40), (80, 24), (50, 14)] {
        render(&mut a, w, h);
    }
    let s = render(&mut a, 140, 40);
    assert!(s.contains("Espacio de Docker") && s.contains("pg-data") && s.contains("n/d"), "{s}");
    a.tab = Tab::Volumes;
    let s = render(&mut a, 140, 40);
    assert!(s.contains("5.00G"), "{s}");

    let mut lv = a.start_task_view("srv $ echo hola".into(), |mut out| async move {
        out("hola".into());
        Ok("✔ listo".into())
    });
    lv.lines.push_back("hola".into());
    lv.ended = Some("✔ terminó (código 0)".into());
    lv.ended_ok = true;
    a.mode = Mode::Logs(Box::new(lv));
    let s = render(&mut a, 100, 30);
    assert!(s.contains("echo hola") && s.contains("terminó (código 0)"), "{s}");
    render(&mut a, 40, 10);
}
