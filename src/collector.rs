//! Recolector en segundo plano: contenedores, estadísticas y métricas del host.

use crate::engine::{Engine, RawStat, timed};
use crate::model::*;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Networks, System};
use tokio::sync::Notify;

pub struct Shared {
    pub notify: Arc<Notify>,
    pub want_extra: AtomicBool,
    pub interval_ms: AtomicU64,
    /// La vista actual muestra uso de disco (Volúmenes, Espacio).
    pub want_usage: AtomicBool,
    /// Forzar un nuevo cálculo de uso de disco (tras limpiar).
    pub usage_now: Arc<Notify>,
}

impl Shared {
    pub fn new(interval_ms: u64) -> Arc<Shared> {
        Arc::new(Shared {
            notify: Arc::new(Notify::new()),
            want_extra: AtomicBool::new(false),
            interval_ms: AtomicU64::new(interval_ms),
            want_usage: AtomicBool::new(false),
            usage_now: Arc::new(Notify::new()),
        })
    }
}

pub struct HostProbe {
    sys: System,
    disks: Disks,
    nets: Networks,
    comps: Components,
    last: Instant,
    prev_net: Option<(u64, u64)>,
    hostname: String,
    os: String,
}

impl HostProbe {
    pub fn new() -> HostProbe {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        HostProbe {
            sys,
            disks: Disks::new_with_refreshed_list(),
            nets: Networks::new_with_refreshed_list(),
            comps: Components::new_with_refreshed_list(),
            last: Instant::now(),
            prev_net: None,
            hostname: System::host_name().unwrap_or_default(),
            os: System::long_os_version().or_else(System::name).unwrap_or_default(),
        }
    }

    pub fn sample(&mut self) -> Host {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.disks.refresh(true);
        self.nets.refresh(true);
        self.comps.refresh(true);
        let dt = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();

        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in self.nets.iter() {
            let n = name.as_str();
            if n == "lo" || n.starts_with("docker") || n.starts_with("br-") || n.starts_with("veth") {
                continue;
            }
            rx += data.total_received();
            tx += data.total_transmitted();
        }
        let (rx_rate, tx_rate) = match self.prev_net {
            Some((prx, ptx)) => (rx.saturating_sub(prx) as f64 / dt, tx.saturating_sub(ptx) as f64 / dt),
            None => (0.0, 0.0),
        };
        self.prev_net = Some((rx, tx));

        let mut disks: Vec<DiskInfo> = Vec::new();
        for d in self.disks.list() {
            let fs = d.file_system().to_string_lossy().to_lowercase();
            if !["ext4", "ext3", "xfs", "btrfs", "f2fs", "zfs", "ntfs", "vfat", "exfat", "fuseblk"].contains(&fs.as_str()) {
                continue;
            }
            let mount = d.mount_point().to_string_lossy().to_string();
            if mount.starts_with("/boot") && mount != "/boot" {
                continue;
            }
            if disks.iter().any(|x| x.total == d.total_space() && x.used == d.total_space().saturating_sub(d.available_space())) {
                continue; // misma partición montada varias veces
            }
            disks.push(DiskInfo {
                mount,
                used: d.total_space().saturating_sub(d.available_space()),
                total: d.total_space(),
            });
        }

        let mut temp: Option<f32> = None;
        let mut best_rank = 0;
        for c in self.comps.list() {
            let label = c.label().to_lowercase();
            let rank = if label.contains("package") || label.contains("tctl") {
                3
            } else if label.contains("cpu") || label.contains("core") || label.contains("k10temp") || label.contains("coretemp") {
                2
            } else {
                1
            };
            if let Some(t) = c.temperature() {
                if t.is_finite() && t > 0.0 && (rank > best_rank || (rank == best_rank && t > temp.unwrap_or(0.0))) {
                    temp = Some(t);
                    best_rank = rank;
                }
            }
        }

        let la = System::load_average();
        Host {
            hostname: self.hostname.clone(),
            os: self.os.clone(),
            cpu: self.sys.global_cpu_usage(),
            cores: self.sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
            cpu_model: self.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            temp_c: temp,
            mem_used: self.sys.used_memory(),
            mem_total: self.sys.total_memory(),
            swap_used: self.sys.used_swap(),
            swap_total: self.sys.total_swap(),
            load: (la.one, la.five, la.fifteen),
            uptime: System::uptime(),
            disks,
            net_rx_rate: rx_rate,
            net_tx_rate: tx_rate,
        }
    }
}

pub async fn run(engine: Arc<Engine>, shared: Arc<Shared>, tx: Tx) {
    let mut probe = HostProbe::new();
    let mut prev: HashMap<String, (RawStat, Instant)> = HashMap::new();
    let mut version = String::new();
    loop {
        match timed(10, engine.list_containers()).await {
            Err(e) => {
                if tx.send(Msg::Disconnected(format!("{e:#}"))).is_err() {
                    return;
                }
                // el daemon pudo volver con otra versión de API
                engine.negotiate().await;
                version.clear();
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            Ok(containers) => {
                if version.is_empty() {
                    version = timed(5, engine.version()).await.unwrap_or_default();
                }
                let running: Vec<String> =
                    containers.iter().filter(|c| c.state == St::Running).map(|c| c.id.clone()).collect();
                let results: Vec<(String, anyhow::Result<RawStat>)> = futures_util::stream::iter(running)
                    .map(|id| {
                        let e = engine.clone();
                        async move {
                            let r = timed(5, e.raw_stats(&id)).await;
                            (id, r)
                        }
                    })
                    .buffer_unordered(8)
                    .collect()
                    .await;
                let now = Instant::now();
                let mut stats: HashMap<String, CStat> = HashMap::new();
                let mut seen = Vec::new();
                for (id, r) in results {
                    let Ok(cur) = r else { continue };
                    let mut st = CStat { mem: cur.mem, mem_limit: cur.limit, rx: cur.rx, tx: cur.tx, ..Default::default() };
                    if let Some((p, pt)) = prev.get(&id) {
                        let dc = cur.cpu_total.saturating_sub(p.cpu_total) as f64;
                        let ds = cur.sys_cpu.saturating_sub(p.sys_cpu) as f64;
                        if ds > 0.0 {
                            st.cpu = (dc / ds * cur.online as f64 * 100.0).max(0.0);
                        }
                        let dt = now.duration_since(*pt).as_secs_f64().max(0.001);
                        st.rx_rate = cur.rx.saturating_sub(p.rx) as f64 / dt;
                        st.tx_rate = cur.tx.saturating_sub(p.tx) as f64 / dt;
                    }
                    prev.insert(id.clone(), (cur, now));
                    seen.push(id.clone());
                    stats.insert(id, st);
                }
                prev.retain(|k, _| seen.contains(k));

                let want = shared.want_extra.load(Ordering::Relaxed);
                let (images, volumes) = if want {
                    (timed(15, engine.list_images(&containers)).await.ok(), timed(15, engine.list_volumes(&containers)).await.ok())
                } else {
                    (None, None)
                };
                // túneles vencidos, o cuyo contenedor ya no es el mismo, se cierran solos
                engine.sweep_tunnels(&containers).await;
                let host = probe.sample();
                let snap = Snapshot { containers, stats, host, images, volumes, docker_version: version.clone() };
                if tx.send(Msg::Snapshot(Box::new(snap))).is_err() {
                    return;
                }
            }
        }
        let ms = shared.interval_ms.load(Ordering::Relaxed);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            _ = shared.notify.notified() => { tokio::time::sleep(Duration::from_millis(250)).await; }
        }
    }
}

/// Uso de disco en segundo plano: solo mientras una vista lo muestra, como mucho cada 30 s
/// (medir volúmenes grandes puede tardar) o al pedirlo tras una limpieza.
pub async fn run_usage(engine: Arc<Engine>, shared: Arc<Shared>, tx: Tx) {
    let mut last: Option<Instant> = None;
    loop {
        let stale = last.map(|t| t.elapsed() > Duration::from_secs(30)).unwrap_or(true);
        if shared.want_usage.load(Ordering::Relaxed) && stale {
            let mut quiet = |_s: String| {};
            match timed(300, engine.disk_usage(&mut quiet)).await {
                Ok(u) => {
                    if tx.send(Msg::Usage(Box::new(u))).is_err() {
                        return;
                    }
                    last = Some(Instant::now());
                }
                Err(_) => last = Some(Instant::now() - Duration::from_secs(20)), // reintento en ~10 s
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = shared.usage_now.notified() => { last = None; }
        }
    }
}
