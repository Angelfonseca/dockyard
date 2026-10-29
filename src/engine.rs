//! Capa de acceso a Docker (bollard): listados, acciones, despliegue, edición, respaldos.

use crate::catalog::{Plan, PortSpec, SvcPlan};
use crate::model::*;
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::models::*;
use bollard::query_parameters::*;
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type Prog<'a> = &'a mut (dyn FnMut(String) + Send);

const HELPER: &str = "alpine:3";

pub struct Engine {
    pub d: Docker,
    /// Dirección del daemon (unix:///…, tcp://…, ssh://…): se muestra y se pasa a `docker compose`/`docker build`.
    pub host: String,
    /// De dónde salió `host` (DOCKER_HOST, contexto, socket encontrado…).
    pub source: String,
    /// Rotación de logs para contenedores nuevos: (max-size, max-file). None = no tocar.
    pub log_rotation: Option<(String, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Start,
    Stop,
    Restart,
    Kill,
    Pause,
    Unpause,
    Remove,
}

#[derive(Clone, Debug, Default)]
pub struct RawStat {
    pub cpu_total: u64,
    pub sys_cpu: u64,
    pub online: u32,
    pub mem: u64,
    pub limit: u64,
    pub rx: u64,
    pub tx: u64,
}

/// Todo lo necesario para recrear un contenedor editado sin perder nada.
#[derive(Clone, Debug)]
pub struct EditSource {
    pub id: String,
    pub name: String,
    pub was_running: bool,
    pub old_image_id: String,
    pub config: ContainerConfig,
    pub host_config: HostConfig,
    pub networks: HashMap<String, EndpointSettings>,
    /// Volúmenes anónimos (creados por VOLUME en la imagen) que hay que reutilizar al recrear.
    pub implicit_mounts: Vec<(String, String, bool)>,
    #[allow(dead_code)]
    pub image_cmd: Vec<String>,
    pub image_entrypoint: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DeployReport {
    pub containers: Vec<String>,
    pub network: Option<String>,
    pub credentials: Vec<(String, String, String)>,
    pub ports: Vec<String>,
    pub note: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateOutcome {
    UpToDate,
    Updated,
}

impl Engine {
    /// Busca el daemon (ver `find_host`), se conecta y negocia la versión de la API.
    pub async fn connect() -> Result<Engine> {
        let (host, source) = find_host();
        let d = Docker::connect_with_host(&host).with_context(|| format!("no se pudo configurar la conexión a Docker ({host})"))?;
        let e = Engine { d, host, source, log_rotation: Some(("10m".into(), 3)) };
        e.negotiate().await;
        Ok(e)
    }

    /// Cliente que nunca conecta (pruebas de la interfaz).
    #[cfg(test)]
    pub fn offline() -> Engine {
        let d = Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION).expect("cliente http");
        Engine { d, host: "tcp://127.0.0.1:1".into(), source: "prueba".into(), log_rotation: None }
    }

    /// Baja la versión de la API si el daemon es más viejo que bollard (si no, todo falla con «client version too new»).
    /// La versión se comparte entre clones del cliente, así que sirve llamarla en cualquier momento.
    pub async fn negotiate(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.d.clone().negotiate_version()).await;
    }

    /// ¿El daemon corre en esta máquina? (puertos, carpetas y shell del host son los de aquí)
    pub fn is_local(&self) -> bool {
        self.host.starts_with("unix://")
    }

    /// `usuario@host` y puerto si el daemon es remoto por SSH.
    pub fn ssh_target(&self) -> Option<(String, Option<u16>)> {
        let rest = self.host.strip_prefix("ssh://")?;
        let rest = rest.split('/').next().unwrap_or(rest);
        match rest.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => Some((h.to_string(), p.parse().ok())),
            _ => Some((rest.to_string(), None)),
        }
    }

    pub async fn version(&self) -> Result<String> {
        let v = self.d.version().await?;
        Ok(v.version.unwrap_or_default())
    }

    // ───────────────────────────── Listados ─────────────────────────────

    pub async fn list_containers(&self) -> Result<Vec<CRow>> {
        let opts = ListContainersOptionsBuilder::default().all(true).build();
        let list = self.d.list_containers(Some(opts)).await?;
        let mut rows: Vec<CRow> = list.into_iter().map(summary_to_row).collect();
        rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(rows)
    }

    pub async fn raw_stats(&self, id: &str) -> Result<RawStat> {
        let opts = StatsOptionsBuilder::default().stream(false).one_shot(true).build();
        let mut s = self.d.stats(id, Some(opts));
        let st = match s.next().await {
            Some(r) => r?,
            None => bail!("sin estadísticas"),
        };
        let cpu = st.cpu_stats.unwrap_or_default();
        let mem = st.memory_stats.unwrap_or_default();
        let inactive = mem
            .stats
            .as_ref()
            .and_then(|m| m.get("inactive_file").or_else(|| m.get("total_inactive_file")))
            .copied()
            .unwrap_or(0);
        let (mut rx, mut tx) = (0u64, 0u64);
        if let Some(nets) = &st.networks {
            for n in nets.values() {
                rx += n.rx_bytes.unwrap_or(0);
                tx += n.tx_bytes.unwrap_or(0);
            }
        }
        Ok(RawStat {
            cpu_total: cpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0),
            sys_cpu: cpu.system_cpu_usage.unwrap_or(0),
            online: cpu.online_cpus.unwrap_or(1).max(1),
            mem: mem.usage.unwrap_or(0).saturating_sub(inactive),
            limit: mem.limit.unwrap_or(0),
            rx,
            tx,
        })
    }

    pub async fn list_images(&self, containers: &[CRow]) -> Result<Vec<IRow>> {
        let opts = ListImagesOptionsBuilder::default().all(false).build();
        let list = self.d.list_images(Some(opts)).await?;
        let mut out: Vec<IRow> = list
            .into_iter()
            .map(|i| {
                let used_by = containers
                    .iter()
                    .filter(|c| c.image_id == i.id)
                    .map(|c| c.name.clone())
                    .collect();
                IRow {
                    id: i.id,
                    tags: i.repo_tags.into_iter().filter(|t| t != "<none>:<none>").collect(),
                    size: i.size.max(0) as u64,
                    created: i.created,
                    used_by,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            let ka = a.tags.first().cloned().unwrap_or_else(|| "~".into());
            let kb = b.tags.first().cloned().unwrap_or_else(|| "~".into());
            ka.to_lowercase().cmp(&kb.to_lowercase())
        });
        Ok(out)
    }

    pub async fn list_volumes(&self, containers: &[CRow]) -> Result<Vec<VRow>> {
        let r = self.d.list_volumes(None::<ListVolumesOptions>).await?;
        let mut out: Vec<VRow> = r
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|v| {
                let used_by = containers
                    .iter()
                    .filter(|c| c.mounts.iter().any(|m| m == &v.name))
                    .map(|c| c.name.clone())
                    .collect();
                VRow { name: v.name, driver: v.driver, mountpoint: v.mountpoint, used_by }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    // ───────────────────────────── Acciones ─────────────────────────────

    pub async fn act(&self, id: &str, act: Act) -> Result<()> {
        match act {
            Act::Start => self.d.start_container(id, None::<StartContainerOptions>).await?,
            Act::Stop => {
                self.d.stop_container(id, Some(StopContainerOptionsBuilder::default().t(15).build())).await?
            }
            Act::Restart => {
                self.d.restart_container(id, Some(RestartContainerOptionsBuilder::default().t(15).build())).await?
            }
            Act::Kill => {
                self.d.kill_container(id, Some(KillContainerOptionsBuilder::default().signal("SIGKILL").build())).await?
            }
            Act::Pause => self.d.pause_container(id).await?,
            Act::Unpause => self.d.unpause_container(id).await?,
            Act::Remove => {
                self.d
                    .remove_container(id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
                    .await?
            }
        }
        Ok(())
    }

    pub async fn inspect_text(&self, id: &str) -> Result<String> {
        let i = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        Ok(serde_json::to_string_pretty(&i)?)
    }

    pub fn log_stream(&self, id: &str, tail: &str, follow: bool, timestamps: bool) -> impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + use<> {
        let opts = LogsOptionsBuilder::default().follow(follow).stdout(true).stderr(true).timestamps(timestamps).tail(tail).build();
        self.d.logs(id, Some(opts))
    }

    // ───────────────────────────── Exec ─────────────────────────────

    /// Ejecuta un comando (sin TTY) y devuelve (salida combinada, código de salida).
    pub async fn exec_collect(&self, id: &str, cmd: Vec<String>, timeout: Duration) -> Result<(String, Option<i64>)> {
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd),
                    ..Default::default()
                },
            )
            .await?
            .id;
        let mut out = String::new();
        let run = async {
            if let StartExecResults::Attached { mut output, .. } = self.d.start_exec(&exec, None).await? {
                while let Some(chunk) = output.next().await {
                    out.push_str(&chunk?.to_string());
                    if out.len() > 2_000_000 {
                        out.push_str("\n… salida truncada …");
                        break;
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(timeout, run).await {
            Ok(r) => r?,
            Err(_) => out.push_str(&format!("\n… tiempo agotado ({}s) …", timeout.as_secs())),
        }
        let code = self.d.inspect_exec(&exec).await.ok().and_then(|i| i.exit_code);
        Ok((out, code))
    }

    /// Ejecuta `sh -c <script>` y escribe stdout en un archivo. Devuelve (bytes, código, cola de stderr).
    pub async fn exec_to_file(&self, id: &str, script: &str, path: &Path) -> Result<(u64, Option<i64>, String)> {
        use std::io::Write;
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
                    ..Default::default()
                },
            )
            .await?
            .id;
        let mut file = std::fs::File::create(path).with_context(|| format!("no se pudo crear {}", path.display()))?;
        let mut written = 0u64;
        let mut errtxt = String::new();
        if let StartExecResults::Attached { mut output, .. } = self.d.start_exec(&exec, None).await? {
            while let Some(chunk) = output.next().await {
                match chunk? {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        file.write_all(&message)?;
                        written += message.len() as u64;
                    }
                    LogOutput::StdErr { message } => {
                        if errtxt.len() < 4000 {
                            errtxt.push_str(&String::from_utf8_lossy(&message));
                        }
                    }
                    _ => {}
                }
            }
        }
        file.flush()?;
        let code = self.exec_code(&exec).await;
        Ok((written, code, errtxt))
    }

    /// Código de salida de un exec; espera a que termine de verdad (el stream cierra un poco antes).
    pub async fn exec_code(&self, exec: &str) -> Option<i64> {
        for _ in 0..50 {
            match self.d.inspect_exec(exec).await {
                Ok(i) if i.running == Some(true) => tokio::time::sleep(Duration::from_millis(100)).await,
                Ok(i) => return i.exit_code,
                Err(_) => return None,
            }
        }
        None
    }

    /// Crea un exec con TTY (stdin opcional). Devuelve el id; luego `start_exec`. Con TTY, cerrar la
    /// conexión manda SIGHUP al proceso: así se puede cancelar un comando.
    pub async fn create_tty_exec(&self, id: &str, cmd: Vec<String>, stdin: bool, size: Option<(u16, u16)>) -> Result<String> {
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdin: Some(stdin),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    console_size: size.map(|(w, h)| vec![h as usize, w as usize]),
                    env: Some(vec!["TERM=xterm-256color".into(), "PAGER=cat".into(), "GIT_PAGER=cat".into()]),
                    cmd: Some(cmd),
                    ..Default::default()
                },
            )
            .await
            .map_err(explain_exec_error)?;
        Ok(exec.id)
    }

    /// Alimenta `sh -c script` con el contenido de un archivo (restauración SQL). Devuelve la cola de la salida.
    pub async fn exec_from_file(&self, id: &str, script: &str, path: &Path, progress: Prog<'_>) -> Result<String> {
        use tokio::io::AsyncWriteExt;
        let size = std::fs::metadata(path).with_context(|| format!("no existe {}", path.display()))?.len();
        // head -c: el comando recibe fin de archivo aunque el daemon no cierre stdin a tiempo
        let full = format!("head -c {size} | ({script})");
        let exec = self
            .d
            .create_exec(
                id,
                ExecConfig {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(vec!["sh".into(), "-c".into(), full]),
                    ..Default::default()
                },
            )
            .await?
            .id;
        let StartExecResults::Attached { mut output, mut input } = self.d.start_exec(&exec, None).await? else {
            bail!("el daemon no adjuntó la entrada");
        };
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = sent.clone();
        let path = path.to_path_buf();
        let writer = tokio::spawn(async move {
            let mut f = tokio::fs::File::open(&path).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut f, &mut buf).await?;
                if n == 0 {
                    break;
                }
                input.write_all(&buf[..n]).await?;
                counter.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            }
            input.flush().await?;
            let _ = input.shutdown().await;
            Ok::<(), std::io::Error>(())
        });
        let mut tail = String::new();
        loop {
            tokio::select! {
                chunk = output.next() => match chunk {
                    Some(Ok(c)) => {
                        tail.push_str(&c.to_string());
                        if tail.len() > 8000 {
                            let mut cut = tail.len() - 6000;
                            while !tail.is_char_boundary(cut) {
                                cut += 1;
                            }
                            tail.drain(..cut);
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                _ = tokio::time::sleep(Duration::from_millis(300)) => {
                    let s = sent.load(std::sync::atomic::Ordering::Relaxed);
                    progress(format!("↑ restaurando  {}% ({}/{})", s * 100 / size.max(1), human_bytes(s), human_bytes(size)));
                }
            }
        }
        let wr = writer.await.map_err(|e| anyhow!("{e}"))?;
        let code = self.exec_code(&exec).await;
        if let Some(c) = code.filter(|c| *c != 0) {
            let clean: Vec<String> = tail.trim().lines().map(strip_ansi).collect();
            bail!("la restauración falló (código {c}):\n{}", clean.join("\n"));
        }
        wr.context("no se pudo enviar el archivo")?;
        Ok(tail)
    }

    // ───────────────────────────── Imágenes ─────────────────────────────

    pub async fn image_exists(&self, image: &str) -> bool {
        self.d.inspect_image(image).await.is_ok()
    }

    pub async fn image_id(&self, image: &str) -> Result<String> {
        let i = self.d.inspect_image(image).await?;
        Ok(i.id.unwrap_or_default())
    }

    /// Huella del contenido de una imagen (capas + configuración). Con el almacén containerd cada build
    /// cambia el ID (las atestaciones llevan fecha) y la imagen vieja deja de poder inspeccionarse al
    /// reetiquetar: hay que tomar la huella antes de construir y compararla después.
    pub async fn image_fingerprint(&self, image: &str) -> Option<String> {
        let i = self.d.inspect_image(image).await.ok()?;
        serde_json::to_string(&(i.root_fs.map(|r| r.layers), i.config)).ok()
    }

    pub async fn pull(&self, image: &str, progress: Prog<'_>) -> Result<()> {
        let (repo, tag) = split_image_tag(image);
        let (from, tag) = if let Some((r, digest)) = image.split_once('@') {
            (r.to_string(), Some(format!("@{digest}")))
        } else {
            (repo.to_string(), Some(tag.unwrap_or("latest").to_string()))
        };
        let mut b = CreateImageOptionsBuilder::default().from_image(&from);
        if let Some(t) = &tag {
            b = b.tag(t.trim_start_matches('@'));
        }
        let mut stream = self.d.create_image(Some(b.build()), None, None);
        let mut layers: HashMap<String, (u64, u64)> = HashMap::new();
        let mut last = std::time::Instant::now();
        while let Some(item) = stream.next().await {
            let info = item.map_err(|e| anyhow!("{e}"))?;
            if let (Some(id), Some(pd)) = (&info.id, &info.progress_detail) {
                if let (Some(cur), Some(tot)) = (pd.current, pd.total) {
                    if tot > 0 {
                        layers.insert(id.clone(), (cur.max(0) as u64, tot as u64));
                    }
                }
            }
            if last.elapsed() > Duration::from_millis(200) {
                last = std::time::Instant::now();
                let (c, t) = layers.values().fold((0u64, 0u64), |a, v| (a.0 + v.0, a.1 + v.1));
                if t > 0 {
                    progress(format!("↓ {image}  {}%  ({}/{})", c * 100 / t, human_bytes(c), human_bytes(t)));
                } else if let Some(s) = &info.status {
                    progress(format!("↓ {image}  {s}"));
                }
            }
        }
        Ok(())
    }

    pub async fn ensure_image(&self, image: &str, progress: Prog<'_>) -> Result<()> {
        if self.image_exists(image).await {
            return Ok(());
        }
        progress(format!("↓ {image}  iniciando descarga…"));
        self.pull(image, progress).await.with_context(|| format!("no se pudo descargar {image}"))
    }

    pub async fn remove_image(&self, id: &str) -> Result<()> {
        self.d
            .remove_image(id, Some(RemoveImageOptionsBuilder::default().force(false).build()), None)
            .await?;
        Ok(())
    }

    /// Borra las imágenes que ningún contenedor usa. `label` limita a las que tengan esa etiqueta.
    pub async fn prune_images(&self, label: Option<&str>) -> Result<(usize, u64)> {
        let mut f = label_filter(label);
        f.insert("dangling".into(), vec!["false".into()]);
        let opts = PruneImagesOptionsBuilder::default().filters(&f).build();
        let r = self.d.prune_images(Some(opts)).await?;
        Ok((r.images_deleted.map(|v| v.len()).unwrap_or(0), r.space_reclaimed.unwrap_or(0).max(0) as u64))
    }

    /// Borra los contenedores detenidos (sus volúmenes se conservan).
    pub async fn prune_containers(&self, label: Option<&str>) -> Result<(usize, u64)> {
        let f = label_filter(label);
        let opts = PruneContainersOptionsBuilder::default().filters(&f).build();
        let r = self.d.prune_containers(Some(opts)).await?;
        Ok((r.containers_deleted.map(|v| v.len()).unwrap_or(0), r.space_reclaimed.unwrap_or(0).max(0) as u64))
    }

    /// Borra las redes que ningún contenedor usa.
    pub async fn prune_networks(&self, label: Option<&str>) -> Result<usize> {
        let f = label_filter(label);
        let opts = PruneNetworksOptionsBuilder::default().filters(&f).build();
        let r = self.d.prune_networks(Some(opts)).await?;
        Ok(r.networks_deleted.map(|v| v.len()).unwrap_or(0))
    }

    /// Borra toda la caché de construcción sin uso. `filter` (clave=valor) la limita.
    pub async fn prune_build(&self, filter: Option<(&str, &str)>) -> Result<u64> {
        let mut b = PruneBuildOptionsBuilder::default().all(true);
        let mut f: HashMap<String, Vec<String>> = HashMap::new();
        if let Some((k, v)) = filter {
            f.insert(k.into(), vec![v.into()]);
            b = b.filters(&f);
        }
        let r = self.d.prune_build(Some(b.build())).await?;
        Ok(r.space_reclaimed.unwrap_or(0).max(0) as u64)
    }

    pub async fn remove_volume(&self, name: &str) -> Result<()> {
        self.d.remove_volume(name, None::<RemoveVolumeOptions>).await?;
        Ok(())
    }

    /// Volúmenes que ningún contenedor (ni detenido) usa. `label` limita a los que tengan esa etiqueta.
    pub async fn orphan_volumes(&self, label: Option<&str>) -> Result<Vec<String>> {
        let mut f = label_filter(label);
        f.insert("dangling".into(), vec!["true".into()]);
        let r = self.d.list_volumes(Some(ListVolumesOptionsBuilder::default().filters(&f).build())).await?;
        let mut v: Vec<String> = r.volumes.unwrap_or_default().into_iter().map(|v| v.name).collect();
        v.sort();
        Ok(v)
    }

    // ───────────────────────────── Espacio ─────────────────────────────

    /// Uso de disco. Con API ≥ 1.52 lo da `system df`; con daemons más viejos se arma con los listados
    /// y el tamaño de cada volumen se mide con un auxiliar (`du`).
    pub async fn disk_usage(&self, progress: Prog<'_>) -> Result<DiskUsage> {
        let df = self.d.df(Some(DataUsageOptionsBuilder::default().verbose(true).build())).await?;
        let mut u = DiskUsage::default();
        let conv = |total: Option<i64>, active: Option<i64>, size: Option<i64>, recl: Option<i64>| Usage {
            count: total.unwrap_or(0).max(0) as u64,
            active: active.unwrap_or(0).max(0) as u64,
            size: size.unwrap_or(0).max(0) as u64,
            reclaimable: recl.unwrap_or(0).max(0) as u64,
            known: true,
        };
        if let Some(x) = &df.build_cache_usage {
            u.build = conv(x.total_count, x.active_count, x.total_size, x.reclaimable);
        }
        let containers = self.list_containers().await?;
        if let (Some(i), Some(c), Some(v)) = (&df.image_usage, &df.container_usage, &df.volume_usage) {
            u.images = conv(i.total_count, i.active_count, i.total_size, i.reclaimable);
            u.containers = conv(c.total_count, c.active_count, c.total_size, c.reclaimable);
            u.volumes = conv(v.total_count, v.active_count, v.total_size, v.reclaimable);
            for item in v.items.clone().unwrap_or_default() {
                if let (Some(n), Some(s)) = (item.get("Name").and_then(|x| x.as_str()), item.pointer("/UsageData/Size").and_then(|x| x.as_i64())) {
                    u.vol_sizes.insert(n.to_string(), s.max(0) as u64);
                }
            }
        } else {
            // API vieja: estimar con los listados
            let imgs = self.list_images(&containers).await?;
            u.images = Usage {
                count: imgs.len() as u64,
                active: imgs.iter().filter(|i| !i.used_by.is_empty()).count() as u64,
                size: imgs.iter().map(|i| i.size).sum(),
                reclaimable: imgs.iter().filter(|i| i.used_by.is_empty()).map(|i| i.size).sum(),
                known: true,
            };
            let opts = ListContainersOptionsBuilder::default().all(true).size(true).build();
            let cs = self.d.list_containers(Some(opts)).await?;
            let running = |c: &ContainerSummary| c.state == Some(ContainerSummaryStateEnum::RUNNING);
            u.containers = Usage {
                count: cs.len() as u64,
                active: cs.iter().filter(|c| running(c)).count() as u64,
                size: cs.iter().map(|c| c.size_rw.unwrap_or(0).max(0) as u64).sum(),
                reclaimable: cs.iter().filter(|c| !running(c)).map(|c| c.size_rw.unwrap_or(0).max(0) as u64).sum(),
                known: true,
            };
            let vols = self.list_volumes(&containers).await?;
            if !vols.is_empty() {
                let binds: Vec<String> = vols.iter().enumerate().map(|(i, v)| format!("{}:/v/{i}:ro", v.name)).collect();
                let mut out = Vec::new();
                self.run_helper(binds, "du -sk /v/* 2>/dev/null; true".into(), &mut |b| {
                    out.extend_from_slice(b);
                    Ok(())
                }, progress)
                .await?;
                for line in String::from_utf8_lossy(&out).lines() {
                    let mut it = line.split_whitespace();
                    let (Some(k), Some(p)) = (it.next(), it.next()) else { continue };
                    let idx: Option<usize> = p.strip_prefix("/v/").and_then(|x| x.parse().ok());
                    if let (Ok(k), Some(v)) = (k.parse::<u64>(), idx.and_then(|i| vols.get(i))) {
                        u.vol_sizes.insert(v.name.clone(), k * 1024);
                    }
                }
            }
            u.volumes = Usage {
                count: vols.len() as u64,
                active: vols.iter().filter(|v| !v.used_by.is_empty()).count() as u64,
                size: u.vol_sizes.values().sum(),
                reclaimable: vols.iter().filter(|v| v.used_by.is_empty()).filter_map(|v| u.vol_sizes.get(&v.name)).sum(),
                known: true,
            };
        }
        if self.is_local() {
            if let Some(root) = self.d.info().await.ok().and_then(|i| i.docker_root_dir) {
                if Path::new(&root).exists() {
                    let disks = sysinfo::Disks::new_with_refreshed_list();
                    let best = disks
                        .list()
                        .iter()
                        .filter(|d| root.starts_with(&*d.mount_point().to_string_lossy()))
                        .max_by_key(|d| d.mount_point().as_os_str().len());
                    if let Some(d) = best {
                        u.root = Some((root, d.total_space().saturating_sub(d.available_space()), d.total_space()));
                    }
                }
            }
        }
        Ok(u)
    }

    /// Credenciales (variables PASS/SECRET/TOKEN y argumentos de contraseña) de un contenedor o de todo su stack.
    pub async fn credentials(&self, target: &str) -> Result<Vec<(String, String, String)>> {
        let all = self.list_containers().await?;
        let members: Vec<&CRow> = all.iter().filter(|c| c.name == target || c.stack.as_deref() == Some(target)).collect();
        if members.is_empty() {
            bail!("no hay contenedor ni stack llamado «{target}»");
        }
        let mut out = Vec::new();
        for c in members {
            let (_, plan) = self.load_edit(&c.id).await?;
            out.extend(plan.credentials());
        }
        Ok(out)
    }

    // ───────────────────────────── Despliegue ─────────────────────────────

    /// Rotación de logs si el driver por defecto del daemon la admite (json-file, local).
    pub async fn log_config(&self) -> Option<HostConfigLogConfig> {
        let (size, files) = self.log_rotation.clone()?;
        if size.is_empty() {
            return None;
        }
        let drv = self.d.info().await.ok()?.logging_driver?;
        if drv != "json-file" && drv != "local" {
            return None;
        }
        let mut c = HashMap::new();
        c.insert("max-size".to_string(), size);
        c.insert("max-file".to_string(), files.max(1).to_string());
        Some(HostConfigLogConfig { typ: Some(drv), config: Some(c) })
    }

    fn svc_body(&self, plan: &Plan, idx: usize, log: &Option<HostConfigLogConfig>) -> Result<ContainerCreateBody> {
        let s = &plan.services[idx];
        let mut exposed: Vec<String> = Vec::new();
        let mut bindings: PortMap = HashMap::new();
        for p in &s.ports {
            let ps = PortSpec::parse(p)?;
            let key = ps.key();
            if !exposed.contains(&key) {
                exposed.push(key.clone());
            }
            if let Some(h) = ps.host {
                let e = bindings.entry(key).or_insert_with(|| Some(Vec::new()));
                if let Some(v) = e.as_mut() {
                    v.push(PortBinding { host_ip: ps.ip.clone(), host_port: Some(h.to_string()) });
                }
            }
        }
        let mut labels = HashMap::new();
        labels.insert("dockyard.managed".to_string(), "true".to_string());
        labels.insert("dockyard.stack".to_string(), plan.name.clone());
        labels.insert("dockyard.template".to_string(), plan.template_id.clone());
        labels.insert("dockyard.role".to_string(), s.role.clone());
        for (k, v) in &plan.labels {
            labels.insert(k.clone(), v.clone());
        }

        let host_config = HostConfig {
            binds: if s.volumes.is_empty() { None } else { Some(s.volumes.clone()) },
            port_bindings: if bindings.is_empty() { None } else { Some(bindings) },
            restart_policy: Some(restart_policy(&s.restart)),
            memory: parse_mem(&s.mem)?,
            nano_cpus: parse_cpus(&s.cpus)?,
            network_mode: plan.network_name(),
            log_config: log.clone(),
            ..Default::default()
        };
        Ok(ContainerCreateBody {
            image: Some(s.image.trim().to_string()),
            env: if s.env.is_empty() { None } else { Some(s.env.clone()) },
            cmd: if s.cmd.is_empty() { None } else { Some(s.cmd.clone()) },
            exposed_ports: if exposed.is_empty() { None } else { Some(exposed) },
            tty: if s.tty { Some(true) } else { None },
            open_stdin: if s.stdin_open { Some(true) } else { None },
            healthcheck: health_config(&s.healthcheck),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        })
    }

    /// Espera a que un servicio recién iniciado esté sano. Error solo si se cae; si tarda o su
    /// healthcheck falla, devuelve un aviso y el despliegue sigue.
    async fn wait_ready(&self, name: &str, limit: Duration, progress: Prog<'_>) -> Result<Option<String>> {
        let t0 = std::time::Instant::now();
        loop {
            let i = self.d.inspect_container(name, None::<InspectContainerOptions>).await?;
            let st = i.state.unwrap_or_default();
            let restarts = i.restart_count.unwrap_or(0);
            let status = st.status.map(|s| s.to_string()).unwrap_or_default();
            if status == "exited" || status == "dead" || restarts >= 3 {
                let mut logs = String::new();
                let mut s = self.log_stream(name, "15", false, false);
                while let Some(Ok(l)) = s.next().await {
                    logs.push_str(&l.to_string());
                }
                let clean: Vec<String> = logs.trim().lines().map(strip_ansi).collect();
                bail!("{name} se detuvo al iniciar (código {}). Últimos logs:\n{}", st.exit_code.unwrap_or(-1), clean.join("\n"));
            }
            let health = st.health.and_then(|h| h.status).map(|s| s.to_string()).unwrap_or_default();
            match health.as_str() {
                "healthy" => return Ok(None),
                "" | "none" => return Ok(None),
                "unhealthy" => return Ok(Some(format!("{name}: su healthcheck falla; revisa sus logs"))),
                _ => {}
            }
            if t0.elapsed() > limit {
                return Ok(Some(format!("{name}: no estuvo listo en {} s; se continuó igual", limit.as_secs())));
            }
            progress(format!("Esperando a que {name} esté listo… {} s", t0.elapsed().as_secs()));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Despliega un plan: descarga imágenes, crea red y contenedores, los arranca.
    /// Si algo falla, deshace lo creado.
    pub async fn deploy(&self, plan: &Plan, progress: Prog<'_>) -> Result<DeployReport> {
        let errs = plan.errors();
        if !errs.is_empty() {
            bail!("{}", errs.join("; "));
        }
        let existing = self.list_containers().await?;
        for i in 0..plan.services.len() {
            let n = plan.container_name(i);
            if existing.iter().any(|c| c.name == n) {
                bail!("ya existe un contenedor llamado «{n}»");
            }
        }
        for s in &plan.services {
            self.ensure_image(&s.image, progress).await?;
        }
        let mut created: Vec<String> = Vec::new();
        let mut net_created: Option<String> = None;
        // volúmenes con nombre que este despliegue va a crear (para limpiarlos si se revierte)
        let before: std::collections::HashSet<String> = self
            .d
            .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
            .await
            .ok()
            .and_then(|r| r.volumes)
            .map(|v| v.into_iter().map(|x| x.name).collect())
            .unwrap_or_default();
        let mut new_vols: Vec<String> = Vec::new();
        for s in &plan.services {
            for v in &s.volumes {
                let src = v.split(':').next().unwrap_or("");
                if !src.is_empty() && !src.starts_with(['/', '.', '~']) && !before.contains(src) && !new_vols.iter().any(|x| x == src) {
                    new_vols.push(src.to_string());
                }
            }
        }
        let log = self.log_config().await;
        let mut warnings: Vec<String> = Vec::new();
        let result: Result<()> = async {
            if let Some(net) = plan.network_name() {
                progress(format!("Creando red {net}…"));
                let mut labels = HashMap::new();
                labels.insert("dockyard.stack".to_string(), plan.name.clone());
                self.d
                    .create_network(NetworkCreateRequest {
                        name: net.clone(),
                        driver: Some("bridge".into()),
                        labels: Some(labels),
                        ..Default::default()
                    })
                    .await
                    .with_context(|| format!("no se pudo crear la red {net}"))?;
                net_created = Some(net);
            }
            for i in 0..plan.services.len() {
                let name = plan.container_name(i);
                progress(format!("Creando {name}…"));
                let body = self.svc_body(plan, i, &log)?;
                let opts = CreateContainerOptionsBuilder::default().name(&name).build();
                self.d.create_container(Some(opts), body).await.with_context(|| format!("al crear {name}"))?;
                created.push(name.clone());
                progress(format!("Iniciando {name}…"));
                if let Err(e) = self.d.start_container(&name, None::<StartContainerOptions>).await {
                    let m = e.to_string();
                    let hint = if m.contains("port is already allocated") || m.contains("address already in use") {
                        " — el puerto ya está ocupado"
                    } else if m.contains("executable file not found") {
                        " — la imagen no tiene ese programa"
                    } else {
                        ""
                    };
                    bail!("al iniciar {name}{hint}: {m}");
                }
                // dependencias (todo lo que no es el principal): esperar a que estén listas
                let s = &plan.services[i];
                if !s.primary && !s.healthcheck.is_empty() {
                    if let Some(w) = self.wait_ready(&name, Duration::from_secs(180), progress).await? {
                        warnings.push(w);
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            for n in created.iter().rev() {
                let _ = self
                    .d
                    .remove_container(n, Some(RemoveContainerOptionsBuilder::default().force(true).v(true).build()))
                    .await;
            }
            if let Some(net) = &net_created {
                let _ = self.d.remove_network(net).await;
            }
            for v in &new_vols {
                let _ = self.remove_volume(v).await;
            }
            return Err(e.context("despliegue cancelado y revertido"));
        }
        let mut ports = Vec::new();
        for (i, s) in plan.services.iter().enumerate() {
            for p in &s.ports {
                if let Ok(ps) = PortSpec::parse(p) {
                    if let Some(h) = ps.host {
                        let ip = ps.ip.as_ref().map(|i| format!("{i}:")).unwrap_or_default();
                        ports.push(format!("{} → {ip}{}:{}/{}", plan.container_name(i), h, ps.container, ps.proto));
                    }
                }
            }
        }
        Ok(DeployReport {
            containers: created,
            network: net_created,
            credentials: plan.credentials(),
            ports,
            note: plan.note.clone(),
            warnings,
        })
    }

    // ───────────────────────────── Edición / recreación ─────────────────────────────

    /// Carga un contenedor existente como `Plan` editable (un solo servicio) + datos para recrearlo.
    pub async fn load_edit(&self, id: &str) -> Result<(EditSource, Plan)> {
        let i = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        let config = i.config.clone().ok_or_else(|| anyhow!("sin configuración"))?;
        let host_config = i.host_config.clone().unwrap_or_default();
        let name = i.name.clone().unwrap_or_default().trim_start_matches('/').to_string();
        let old_image_id = i.image.clone().unwrap_or_default();

        // Lo que la imagen ya define (para no mostrar PATH, etc. como si fuera del usuario)
        let (img_env, img_cmd, img_ep) = match self.d.inspect_image(&old_image_id).await {
            Ok(img) => {
                let c = img.config.unwrap_or_default();
                (c.env.unwrap_or_default(), c.cmd.unwrap_or_default(), c.entrypoint.unwrap_or_default())
            }
            Err(_) => (vec![], vec![], vec![]),
        };

        let env: Vec<String> =
            config.env.clone().unwrap_or_default().into_iter().filter(|e| !img_env.contains(e)).collect();
        let cmd_now = config.cmd.clone().unwrap_or_default();
        let cmd = if cmd_now == img_cmd { vec![] } else { cmd_now };

        // Puertos desde PortBindings
        let mut ports = Vec::new();
        if let Some(pm) = &host_config.port_bindings {
            let mut keys: Vec<&String> = pm.keys().collect();
            keys.sort();
            for k in keys {
                let (cport, proto) = k.split_once('/').unwrap_or((k.as_str(), "tcp"));
                if let Some(Some(list)) = pm.get(k) {
                    for b in list {
                        let hp = b.host_port.clone().unwrap_or_default();
                        if hp.is_empty() {
                            continue;
                        }
                        let ip = b.host_ip.clone().filter(|x| !x.is_empty() && x != "0.0.0.0" && x != "::");
                        let ps = PortSpec {
                            ip,
                            host: hp.parse().ok(),
                            container: cport.parse().unwrap_or(0),
                            proto: proto.to_string(),
                        };
                        ports.push(ps.to_text());
                    }
                }
            }
        }

        let binds = host_config.binds.clone().unwrap_or_default();
        // Mounts que existen pero no están declarados ni en Binds ni en HostConfig.Mounts → implícitos
        let declared: Vec<String> = host_config
            .mounts
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| m.target)
            .chain(binds.iter().filter_map(|b| b.split(':').nth(1).map(|s| s.to_string())))
            .collect();
        let mut implicit = Vec::new();
        for m in i.mounts.clone().unwrap_or_default() {
            let is_vol = m.typ.as_deref() == Some("volume");
            let dest = m.destination.clone().unwrap_or_default();
            if is_vol && !dest.is_empty() && !declared.contains(&dest) {
                if let Some(n) = m.name {
                    implicit.push((n, dest, m.rw.unwrap_or(true)));
                }
            }
        }

        let restart = host_config
            .restart_policy
            .as_ref()
            .and_then(|r| r.name)
            .map(|n| match n {
                RestartPolicyNameEnum::ALWAYS => "always",
                RestartPolicyNameEnum::UNLESS_STOPPED => "unless-stopped",
                RestartPolicyNameEnum::ON_FAILURE => "on-failure",
                _ => "no",
            })
            .unwrap_or("no")
            .to_string();

        let mut networks = i.network_settings.clone().and_then(|n| n.networks).unwrap_or_default();
        for ep in networks.values_mut() {
            if let Some(al) = ep.aliases.as_mut() {
                al.retain(|a| a.len() != 12 || !id_matches(&i.id, a));
            }
        }

        let svc = SvcPlan {
            role: "app".into(),
            primary: true,
            image: config.image.clone().unwrap_or_default(),
            ports,
            env,
            volumes: binds,
            restart,
            mem: mem_to_text(host_config.memory.unwrap_or(0)),
            cmd,
            tty: config.tty.unwrap_or(false),
            stdin_open: config.open_stdin.unwrap_or(false),
            cpus: cpus_to_text(host_config.nano_cpus.unwrap_or(0)),
            healthcheck: String::new(), // se conserva el de la configuración original
        };
        let plan = Plan {
            name: name.clone(),
            template_id: String::new(),
            template_name: format!("Editar «{name}»"),
            note: String::new(),
            services: vec![svc],
            labels: Default::default(),
        };
        let src = EditSource {
            id: i.id.clone().unwrap_or_else(|| id.to_string()),
            name,
            was_running: i.state.as_ref().and_then(|s| s.running).unwrap_or(false),
            old_image_id,
            config,
            host_config,
            networks,
            implicit_mounts: implicit,
            image_cmd: img_cmd,
            image_entrypoint: img_ep,
        };
        Ok((src, plan))
    }

    fn recreate_body(&self, src: &EditSource, plan: &Plan, log: Option<HostConfigLogConfig>) -> Result<ContainerCreateBody> {
        let s = &plan.services[0];
        let mut body: ContainerCreateBody = serde_json::from_value(serde_json::to_value(&src.config)?)
            .context("no se pudo reutilizar la configuración original")?;
        body.image = Some(s.image.trim().to_string());
        // Docker combina las variables de la imagen con estas; solo enviamos las del usuario
        body.env = if s.env.is_empty() { None } else { Some(s.env.clone()) };
        body.cmd = if s.cmd.is_empty() { None } else { Some(s.cmd.clone()) };
        if body.entrypoint.as_ref() == Some(&src.image_entrypoint) {
            body.entrypoint = None; // que mande la imagen (por si cambió el tag)
        }
        if let Some(h) = &body.hostname {
            if h.len() == 12 && id_matches(&Some(src.id.clone()), h) {
                body.hostname = None;
            }
        }
        body.tty = Some(s.tty);
        body.open_stdin = Some(s.stdin_open);

        let mut exposed = body.exposed_ports.take().unwrap_or_default();
        let mut bindings: PortMap = HashMap::new();
        for p in &s.ports {
            let ps = PortSpec::parse(p)?;
            let key = ps.key();
            if !exposed.contains(&key) {
                exposed.push(key.clone());
            }
            if let Some(h) = ps.host {
                let e = bindings.entry(key).or_insert_with(|| Some(Vec::new()));
                if let Some(v) = e.as_mut() {
                    v.push(PortBinding { host_ip: ps.ip.clone(), host_port: Some(h.to_string()) });
                }
            }
        }
        body.exposed_ports = if exposed.is_empty() { None } else { Some(exposed) };

        let mut hc = src.host_config.clone();
        hc.port_bindings = if bindings.is_empty() { None } else { Some(bindings) };
        let mut binds = s.volumes.clone();
        for (vol, dest, rw) in &src.implicit_mounts {
            binds.push(format!("{vol}:{dest}{}", if *rw { "" } else { ":ro" }));
        }
        hc.binds = if binds.is_empty() { None } else { Some(binds) };
        hc.restart_policy = Some(restart_policy(&s.restart));
        hc.memory = parse_mem(&s.mem)?;
        hc.nano_cpus = parse_cpus(&s.cpus)?;
        // rotación de logs solo si el contenedor no tenía opciones propias y usa el driver por defecto
        if let Some(l) = log {
            let cur = hc.log_config.clone().unwrap_or_default();
            let same_driver = cur.typ.as_deref().map(|t| t.is_empty() || Some(t) == l.typ.as_deref()).unwrap_or(true);
            if same_driver && cur.config.map(|c| c.is_empty()).unwrap_or(true) {
                hc.log_config = Some(l);
            }
        }
        // la red se reconecta aparte
        let primary_net = hc.network_mode.clone();
        let mut endpoints = HashMap::new();
        if let Some(pn) = &primary_net {
            if let Some(ep) = src.networks.get(pn) {
                endpoints.insert(pn.clone(), slim_endpoint(ep));
            }
        }
        if !endpoints.is_empty() {
            body.networking_config = Some(NetworkingConfig { endpoints_config: Some(endpoints) });
        }
        body.host_config = Some(hc);
        Ok(body)
    }

    /// Reemplaza un contenedor por una versión editada. Con reversión si falla.
    pub async fn recreate(&self, src: &EditSource, plan: &Plan, progress: Prog<'_>) -> Result<String> {
        let errs = plan.errors();
        if !errs.is_empty() {
            bail!("{}", errs.join("; "));
        }
        let s = &plan.services[0];
        self.ensure_image(&s.image, progress).await?;
        let log = self.log_config().await;
        let body = self.recreate_body(src, plan, log)?;

        let old_tmp = format!("{}-old-{}", src.name, &utc_stamp()[9..]);
        if src.was_running {
            progress(format!("Deteniendo {}…", src.name));
            self.d
                .stop_container(&src.id, Some(StopContainerOptionsBuilder::default().t(15).build()))
                .await
                .context("no se pudo detener el contenedor original")?;
        }
        self.d
            .rename_container(&src.id, RenameContainerOptionsBuilder::default().name(&old_tmp).build())
            .await
            .context("no se pudo renombrar el original")?;

        let rollback = |why: anyhow::Error| async move {
            let _ = self
                .d
                .rename_container(&src.id, RenameContainerOptionsBuilder::default().name(&src.name).build())
                .await;
            if src.was_running {
                let _ = self.d.start_container(&src.id, None::<StartContainerOptions>).await;
            }
            why.context("cambios revertidos: el contenedor original sigue como estaba")
        };

        progress(format!("Creando {}…", src.name));
        let opts = CreateContainerOptionsBuilder::default().name(&src.name).build();
        let created = match self.d.create_container(Some(opts), body).await {
            Ok(c) => c,
            Err(e) => return Err(rollback(anyhow!("{e}")).await),
        };
        // redes adicionales
        let primary = src.host_config.network_mode.clone();
        for (net, ep) in &src.networks {
            if Some(net.as_str()) == primary.as_deref() || net == "none" || net == "host" {
                continue;
            }
            let _ = self
                .d
                .connect_network(
                    net,
                    NetworkConnectRequest { container: created.id.clone(), endpoint_config: Some(slim_endpoint(ep)) },
                )
                .await;
        }
        if src.was_running {
            progress(format!("Iniciando {}…", src.name));
            if let Err(e) = self.d.start_container(&created.id, None::<StartContainerOptions>).await {
                let _ = self
                    .d
                    .remove_container(&created.id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
                    .await;
                return Err(rollback(anyhow!("{e}")).await);
            }
        }
        // conservar volúmenes: v=false
        let _ = self
            .d
            .remove_container(&src.id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
            .await;
        Ok(created.id)
    }

    /// Descarga la imagen actual de un contenedor y lo recrea si cambió.
    pub async fn update(&self, id: &str, progress: Prog<'_>) -> Result<UpdateOutcome> {
        let (src, plan) = self.load_edit(id).await?;
        let image = plan.services[0].image.clone();
        self.pull(&image, progress).await.with_context(|| format!("no se pudo descargar {image} (¿imagen local?)"))?;
        let new_id = self.image_id(&image).await?;
        if new_id == src.old_image_id {
            return Ok(UpdateOutcome::UpToDate);
        }
        self.recreate(&src, &plan, progress).await?;
        Ok(UpdateOutcome::Updated)
    }

    /// Elimina todos los contenedores de un stack (etiqueta dockyard.stack) y su red.
    pub async fn down_stack(&self, stack: &str, volumes: bool) -> Result<usize> {
        let all = self.list_containers().await?;
        let members: Vec<&CRow> = all.iter().filter(|c| c.stack.as_deref() == Some(stack)).collect();
        if members.is_empty() {
            bail!("no hay contenedores en el stack «{stack}»");
        }
        let mut vols: Vec<String> = Vec::new();
        for c in &members {
            let i = self.d.inspect_container(&c.id, None::<InspectContainerOptions>).await?;
            for m in i.mounts.unwrap_or_default() {
                if m.typ.as_deref() == Some("volume") {
                    if let Some(n) = m.name {
                        if !vols.contains(&n) {
                            vols.push(n);
                        }
                    }
                }
            }
        }
        for c in &members {
            self.act(&c.id, Act::Remove).await?;
        }
        // redes del stack: la de dockyard («<stack>-net») y las de Compose («<proyecto>_default», etc.)
        for n in self.d.list_networks(None::<ListNetworksOptions>).await.unwrap_or_default() {
            let labels = n.labels.unwrap_or_default();
            let name = n.name.unwrap_or_default();
            let mine = labels.get("dockyard.stack").map(|s| s == stack).unwrap_or(false)
                || labels.get("com.docker.compose.project").map(|s| s == stack).unwrap_or(false)
                || name == format!("{stack}-net");
            if mine {
                let _ = self.d.remove_network(&name).await;
            }
        }
        if volumes {
            for v in vols {
                let _ = self.remove_volume(&v).await;
            }
        }
        Ok(members.len())
    }

    // ───────────────────────────── Respaldos ─────────────────────────────

    /// Respaldo lógico de una base de datos usando el comando `dump` de la plantilla.
    pub async fn backup_dump(&self, id: &str, name: &str, script: &str, ext: &str, dir: &Path) -> Result<(PathBuf, u64)> {
        std::fs::create_dir_all(dir).with_context(|| format!("no se pudo crear {}", dir.display()))?;
        let path = dir.join(format!("{name}-{}.{ext}", utc_stamp()));
        let (bytes, code, err) = self.exec_to_file(id, script, &path).await?;
        if code.unwrap_or(0) != 0 || bytes == 0 {
            let _ = std::fs::remove_file(&path);
            bail!("el respaldo falló (código {:?}): {}", code, err.trim());
        }
        Ok((path, bytes))
    }

    /// Crea (sin iniciar) un contenedor auxiliar alpine con `binds`, sin red. `script` se ejecuta con sh -c al iniciarlo.
    pub async fn create_helper(&self, binds: Vec<String>, script: Option<String>, progress: Prog<'_>) -> Result<String> {
        self.ensure_image(HELPER, progress).await?;
        let mut labels = HashMap::new();
        labels.insert("dockyard.helper".to_string(), "true".to_string());
        let body = ContainerCreateBody {
            image: Some(HELPER.into()),
            cmd: Some(match script {
                Some(s) => vec!["sh".into(), "-c".into(), s],
                None => vec!["true".into()],
            }),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            labels: Some(labels),
            host_config: Some(HostConfig { binds: Some(binds), network_mode: Some("none".into()), ..Default::default() }),
            ..Default::default()
        };
        let name = format!("dockyard-helper-{}", rand_alnum(8).to_lowercase());
        self.d
            .create_container(Some(CreateContainerOptionsBuilder::default().name(&name).build()), body)
            .await
            .context("no se pudo crear el contenedor auxiliar")?;
        Ok(name)
    }

    pub async fn remove_helper(&self, name: &str) {
        let _ = self.d.remove_container(name, Some(RemoveContainerOptionsBuilder::default().force(true).build())).await;
    }

    /// Código de salida de un contenedor que ya terminó (o termina).
    async fn wait_exit(&self, name: &str) -> Result<i64> {
        let mut w = self.d.wait_container(name, None::<WaitContainerOptions>);
        let mut code = 0;
        while let Some(r) = w.next().await {
            match r {
                Ok(r) => code = r.status_code,
                Err(bollard::errors::Error::DockerContainerWaitError { code: c, .. }) => code = c,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(code)
    }

    /// Corre `sh -c script` en un auxiliar con `binds`. Cada trozo de stdout va a `sink` (la salida
    /// viaja por la API, así funciona igual con un daemon remoto). Falla si el script termina con error.
    pub async fn run_helper(
        &self,
        binds: Vec<String>,
        script: String,
        sink: &mut (dyn FnMut(&[u8]) -> Result<()> + Send),
        progress: Prog<'_>,
    ) -> Result<()> {
        let name = self.create_helper(binds, Some(script), progress).await?;
        let res: Result<()> = async {
            let opts = AttachContainerOptionsBuilder::default().stream(true).stdout(true).stderr(true).build();
            let mut att = self.d.attach_container(&name, Some(opts)).await?;
            self.d.start_container(&name, None::<StartContainerOptions>).await?;
            let mut err = String::new();
            while let Some(chunk) = att.output.next().await {
                match chunk? {
                    LogOutput::StdOut { message } => sink(&message)?,
                    LogOutput::StdErr { message } => {
                        if err.len() < 4000 {
                            err.push_str(&String::from_utf8_lossy(&message));
                        }
                    }
                    _ => {}
                }
            }
            let code = self.wait_exit(&name).await?;
            if code != 0 {
                bail!("el auxiliar terminó con código {code}: {}", err.trim());
            }
            Ok(())
        }
        .await;
        self.remove_helper(&name).await;
        res
    }

    /// Empaqueta un volumen en un .tar.gz dentro de `dir` (local, aunque el daemon sea remoto).
    pub async fn backup_volume(&self, volume: &str, dir: &Path, progress: Prog<'_>) -> Result<(PathBuf, u64)> {
        use std::io::Write;
        std::fs::create_dir_all(dir).with_context(|| format!("no se pudo crear {}", dir.display()))?;
        let path = dir.join(format!("{volume}-{}.tar.gz", utc_stamp()));
        let mut file = std::fs::File::create(&path).with_context(|| format!("no se pudo crear {}", path.display()))?;
        let mut n = 0u64;
        let r = {
            let mut sink = |b: &[u8]| -> Result<()> {
                file.write_all(b)?;
                n += b.len() as u64;
                Ok(())
            };
            self.run_helper(vec![format!("{volume}:/data:ro")], "tar czf - -C /data .".into(), &mut sink, progress).await
        };
        let r = r.and_then(|_| file.sync_all().map_err(Into::into));
        if let Err(e) = r {
            let _ = std::fs::remove_file(&path);
            return Err(e.context(format!("respaldo de «{volume}» cancelado")));
        }
        Ok((path, n))
    }

    /// Restaura un .tar.gz (o .tar) sobre un volumen; lo crea si no existe. El daemon descomprime y conserva
    /// dueños y permisos. `clean` vacía el volumen antes; si no, los archivos que sobren se quedan.
    pub async fn restore_volume(&self, volume: &str, file: &Path, clean: bool, progress: Prog<'_>) -> Result<()> {
        let meta = std::fs::metadata(file).with_context(|| format!("no existe {}", file.display()))?;
        if !meta.is_file() {
            bail!("{} no es un archivo", file.display());
        }
        if clean {
            progress(format!("Vaciando {volume}…"));
            self.run_helper(vec![format!("{volume}:/data")], "find /data -mindepth 1 -delete".into(), &mut |_| Ok(()), progress)
                .await
                .context("no se pudo vaciar el volumen")?;
        }
        let name = self.create_helper(vec![format!("{volume}:/data")], None, progress).await?;
        let total = meta.len();
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let res: Result<()> = async {
            let f = tokio::fs::File::open(file).await?;
            let counter = sent.clone();
            let stream = futures_util::stream::unfold(f, move |mut f| {
                let counter = counter.clone();
                async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = vec![0u8; 256 * 1024];
                    match f.read(&mut buf).await {
                        Ok(0) => None,
                        Ok(n) => {
                            buf.truncate(n);
                            counter.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                            Some((Ok(bytes::Bytes::from(buf)), f))
                        }
                        Err(e) => Some((Err(e), f)),
                    }
                }
            });
            let opts = UploadToContainerOptionsBuilder::default().path("/data").build();
            let up = self.d.upload_to_container(&name, Some(opts), bollard::body_try_stream(stream));
            tokio::pin!(up);
            loop {
                tokio::select! {
                    r = &mut up => { r.context("el daemon rechazó el archivo (¿es un .tar.gz válido?)")?; break; }
                    _ = tokio::time::sleep(Duration::from_millis(300)) => {
                        let s = sent.load(std::sync::atomic::Ordering::Relaxed);
                        progress(format!("↑ {volume}  {}% ({}/{})", s * 100 / total.max(1), human_bytes(s), human_bytes(total)));
                    }
                }
            }
            Ok(())
        }
        .await;
        self.remove_helper(&name).await;
        res
    }

    /// Carpetas y archivos más pesados de un volumen, y su listado.
    pub async fn volume_du(&self, volume: &str, progress: Prog<'_>) -> Result<String> {
        let script = "cd /data && echo '== Uso total ==' && du -sh . && echo && echo '== Lo más pesado (3 niveles) ==' \
                      && du -ah -d 3 . 2>/dev/null | sort -rh | head -n 40 && echo && echo '== Contenido ==' && ls -la";
        let mut out: Vec<u8> = Vec::new();
        self.run_helper(
            vec![format!("{volume}:/data:ro")],
            script.into(),
            &mut |b| {
                if out.len() < 2_000_000 {
                    out.extend_from_slice(b);
                }
                Ok(())
            },
            progress,
        )
        .await?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Copia exacta de un volumen en otro nuevo.
    pub async fn clone_volume(&self, src: &str, dst: &str, progress: Prog<'_>) -> Result<()> {
        if !crate::catalog::valid_name(dst) {
            bail!("nombre de volumen inválido: «{dst}»");
        }
        let vols = self.d.list_volumes(None::<ListVolumesOptions>).await?.volumes.unwrap_or_default();
        let Some(orig) = vols.iter().find(|v| v.name == src) else { bail!("no existe el volumen «{src}»") };
        if vols.iter().any(|v| v.name == dst) {
            bail!("ya existe un volumen llamado «{dst}»");
        }
        self.d
            .create_volume(VolumeCreateRequest {
                name: Some(dst.to_string()),
                driver: Some(orig.driver.clone()),
                labels: Some(orig.labels.clone()),
                ..Default::default()
            })
            .await?;
        progress(format!("Copiando {src} → {dst}…"));
        let r = self
            .run_helper(vec![format!("{src}:/from:ro"), format!("{dst}:/to")], "cp -a /from/. /to/".into(), &mut |_| Ok(()), progress)
            .await;
        if let Err(e) = r {
            let _ = self.remove_volume(dst).await;
            return Err(e.context("copia cancelada; se borró el volumen nuevo"));
        }
        Ok(())
    }

    // ───────────────────────────── Archivos (API de archivos) ─────────────────────────────

    /// Lee un archivo de un contenedor (activo o no). Devuelve (contenido, cabecera tar con permisos y dueño).
    /// Si no existe, contenido vacío y cabecera por defecto (0644, root).
    pub async fn read_file(&self, container: &str, path: &str) -> Result<(Vec<u8>, tar::Header, bool)> {
        const MAX: usize = 50 * 1024 * 1024;
        let opts = DownloadFromContainerOptionsBuilder::default().path(path).build();
        let mut s = self.d.download_from_container(container, Some(opts));
        let mut raw: Vec<u8> = Vec::new();
        while let Some(chunk) = s.next().await {
            match chunk {
                Ok(b) => {
                    raw.extend_from_slice(&b);
                    if raw.len() > MAX + 1024 * 1024 {
                        bail!("el archivo es demasiado grande para editarlo aquí (más de 50 MB)");
                    }
                }
                Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => {
                    let mut h = tar::Header::new_gnu();
                    h.set_mode(0o644);
                    h.set_entry_type(tar::EntryType::Regular);
                    return Ok((Vec::new(), h, true));
                }
                Err(e) => return Err(e.into()),
            }
        }
        let mut ar = tar::Archive::new(raw.as_slice());
        let mut entry = ar.entries()?.next().ok_or_else(|| anyhow!("respuesta vacía del daemon"))??;
        match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            tar::EntryType::Directory => bail!("«{path}» es una carpeta"),
            tar::EntryType::Symlink => {
                let to = entry.link_name().ok().flatten().map(|p| p.display().to_string()).unwrap_or_default();
                bail!("«{path}» es un enlace a «{to}»: edita el destino")
            }
            _ => bail!("«{path}» no es un archivo normal"),
        }
        let header = entry.header().clone();
        let mut content = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut content)?;
        Ok((content, header, false))
    }

    /// Escribe un archivo en un contenedor conservando permisos y dueño de `header`.
    pub async fn write_file(&self, container: &str, path: &str, content: &[u8], header: &tar::Header) -> Result<()> {
        let p = Path::new(path);
        let dir = p.parent().map(|d| d.to_string_lossy().to_string()).filter(|d| !d.is_empty()).unwrap_or_else(|| "/".into());
        let fname = p.file_name().ok_or_else(|| anyhow!("ruta inválida: «{path}»"))?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(header.mode().unwrap_or(0o644));
        h.set_uid(header.uid().unwrap_or(0));
        h.set_gid(header.gid().unwrap_or(0));
        h.set_mtime(now_secs().max(0) as u64);
        h.set_size(content.len() as u64);
        let mut b = tar::Builder::new(Vec::new());
        b.append_data(&mut h, fname, content)?;
        let data = b.into_inner()?;
        let opts = UploadToContainerOptionsBuilder::default().path(&dir).build();
        self.d
            .upload_to_container(container, Some(opts), bollard::body_full(bytes::Bytes::from(data)))
            .await
            .with_context(|| format!("no se pudo escribir en {dir} (¿existe la carpeta?)"))?;
        Ok(())
    }

    /// Vigila eventos de Docker y avisa para refrescar al instante.
    pub fn watch_events(self: &std::sync::Arc<Self>, notify: std::sync::Arc<tokio::sync::Notify>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let mut s = me.d.events(None::<EventsOptions>);
                while let Some(ev) = s.next().await {
                    if ev.is_err() {
                        break;
                    }
                    notify.notify_one();
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }
}

// ───────────────────────────── Auxiliares ─────────────────────────────

/// Espera como mucho `secs`; si el daemon no contesta, error claro en vez de quedarse colgado.
pub async fn timed<T>(secs: u64, f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .unwrap_or_else(|_| Err(anyhow!("Docker no responde (más de {secs} s)")))
}

/// Dónde está el daemon: DOCKER_HOST → contexto de `docker context` → sockets conocidos
/// (Docker, Docker Desktop, rootless, Colima, OrbStack, Rancher Desktop, Podman). Devuelve (host, origen).
pub fn find_host() -> (String, String) {
    if let Ok(h) = std::env::var("DOCKER_HOST") {
        if !h.trim().is_empty() {
            return (h.trim().to_string(), "DOCKER_HOST".into());
        }
    }
    let cfg_dir = std::env::var_os("DOCKER_CONFIG").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".docker"));
    if let Some((name, h)) = context_host(&cfg_dir, std::env::var("DOCKER_CONTEXT").ok()) {
        return (h, format!("contexto «{name}»"));
    }
    let cands = socket_candidates();
    // primero uno que acepte conexiones; si ninguno, el primero que exista
    for p in &cands {
        if std::os::unix::net::UnixStream::connect(p).is_ok() {
            return (format!("unix://{p}"), "socket encontrado".into());
        }
    }
    for p in &cands {
        if Path::new(p).exists() {
            return (format!("unix://{p}"), "socket encontrado (sin respuesta)".into());
        }
    }
    ("unix:///var/run/docker.sock".into(), "por defecto".into())
}

/// Host del contexto activo de `docker context` (None si es «default» o no hay).
fn context_host(cfg_dir: &Path, env_ctx: Option<String>) -> Option<(String, String)> {
    let name = match env_ctx.filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => {
            let txt = std::fs::read_to_string(cfg_dir.join("config.json")).ok()?;
            let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
            v.get("currentContext")?.as_str()?.to_string()
        }
    };
    if name.is_empty() || name == "default" {
        return None;
    }
    // contexts/meta/<sha256(nombre)>/meta.json: se recorre en vez de calcular el hash
    for ent in std::fs::read_dir(cfg_dir.join("contexts").join("meta")).ok()?.flatten() {
        let Ok(txt) = std::fs::read_to_string(ent.path().join("meta.json")) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        if v.get("Name").and_then(|n| n.as_str()) == Some(name.as_str()) {
            let h = v.pointer("/Endpoints/docker/Host")?.as_str()?.to_string();
            return Some((name, h));
        }
    }
    None
}

fn socket_candidates() -> Vec<String> {
    let home = home_dir();
    let home = home.display();
    let uid = unsafe { libc::getuid() };
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{uid}"));
    vec![
        "/var/run/docker.sock".into(),
        format!("{home}/.docker/run/docker.sock"),
        format!("{home}/.docker/desktop/docker.sock"),
        format!("{xdg}/docker.sock"),
        format!("{home}/.colima/default/docker.sock"),
        format!("{home}/.orbstack/run/docker.sock"),
        format!("{home}/.rd/docker.sock"),
        format!("{xdg}/podman/podman.sock"),
        "/run/podman/podman.sock".into(),
    ]
}

fn id_matches(full: &Option<String>, short: &str) -> bool {
    full.as_deref().map(|f| f.starts_with(short)).unwrap_or(false)
}

fn slim_endpoint(ep: &EndpointSettings) -> EndpointSettings {
    EndpointSettings {
        aliases: ep.aliases.clone().filter(|a| !a.is_empty()),
        ipam_config: ep.ipam_config.clone(),
        links: ep.links.clone(),
        ..Default::default()
    }
}

fn restart_policy(name: &str) -> RestartPolicy {
    RestartPolicy {
        name: Some(match name {
            "always" => RestartPolicyNameEnum::ALWAYS,
            "unless-stopped" => RestartPolicyNameEnum::UNLESS_STOPPED,
            "on-failure" => RestartPolicyNameEnum::ON_FAILURE,
            _ => RestartPolicyNameEnum::NO,
        }),
        maximum_retry_count: None,
    }
}

fn label_filter(label: Option<&str>) -> HashMap<String, Vec<String>> {
    let mut f = HashMap::new();
    if let Some(l) = label {
        f.insert("label".to_string(), vec![l.to_string()]);
    }
    f
}

fn health_config(cmd: &str) -> Option<HealthConfig> {
    if cmd.trim().is_empty() {
        return None;
    }
    const S: i64 = 1_000_000_000;
    Some(HealthConfig {
        test: Some(vec!["CMD-SHELL".into(), cmd.to_string()]),
        interval: Some(15 * S),
        timeout: Some(5 * S),
        retries: Some(5),
        start_period: Some(120 * S),
        start_interval: Some(2 * S),
    })
}

fn explain_exec_error(e: bollard::errors::Error) -> anyhow::Error {
    let s = e.to_string();
    if s.contains("is not running") || s.contains("is paused") {
        anyhow!("el contenedor no está activo")
    } else {
        anyhow!(s)
    }
}

/// Mensaje claro cuando la imagen no trae el programa pedido (imágenes distroless, scratch…).
pub fn missing_binary_hint(output: &str) -> Option<String> {
    if output.contains("executable file not found") || output.contains("no such file or directory: unknown") {
        Some("La imagen no incluye ese programa (¿imagen distroless o scratch sin shell?). Usa i para inspeccionarla o F para editar archivos.".into())
    } else {
        None
    }
}

fn summary_to_row(c: ContainerSummary) -> CRow {
    let name = c
        .names
        .as_ref()
        .and_then(|n| n.first())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_default();
    let state = match c.state {
        Some(ContainerSummaryStateEnum::RUNNING) => St::Running,
        Some(ContainerSummaryStateEnum::PAUSED) => St::Paused,
        Some(ContainerSummaryStateEnum::RESTARTING) => St::Restarting,
        Some(ContainerSummaryStateEnum::CREATED) => St::Created,
        Some(ContainerSummaryStateEnum::EXITED) => St::Exited,
        Some(ContainerSummaryStateEnum::DEAD) => St::Dead,
        _ => St::Other,
    };
    let status = c.status.clone().unwrap_or_default();
    let health: Option<crate::model::Health> = if status.contains("(unhealthy)") {
        Some(crate::model::Health::Unhealthy)
    } else if status.contains("(healthy)") {
        Some(crate::model::Health::Healthy)
    } else if status.contains("health: starting") {
        Some(crate::model::Health::Starting)
    } else {
        None
    };
    let mut ports: Vec<String> = Vec::new();
    let mut published: Vec<(u16, String)> = Vec::new();
    for p in c.ports.clone().unwrap_or_default() {
        let proto = p.typ.map(|t| t.to_string()).unwrap_or_else(|| "tcp".into());
        if let Some(h) = p.public_port {
            if !published.contains(&(h, proto.clone())) {
                published.push((h, proto.clone()));
            }
        }
        let sfx = if proto == "tcp" { String::new() } else { format!("/{proto}") };
        let s = match p.public_port {
            Some(h) => {
                let ip = p.ip.clone().filter(|i| i != "0.0.0.0" && i != "::" && !i.is_empty());
                match ip {
                    Some(ip) => format!("{ip}:{h}→{}{sfx}", p.private_port),
                    None => format!("{h}→{}{sfx}", p.private_port),
                }
            }
            None => format!("{}{sfx}", p.private_port),
        };
        if !ports.contains(&s) {
            ports.push(s);
        }
    }
    let labels = c.labels.clone().unwrap_or_default();
    let stack = labels
        .get("dockyard.stack")
        .or_else(|| labels.get("com.docker.compose.project"))
        .cloned();
    let mounts = c
        .mounts
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| match m.typ.as_deref() {
            Some("volume") => m.name,
            _ => m.source,
        })
        .collect();
    let networks = c
        .network_settings
        .and_then(|n| n.networks)
        .map(|n| {
            let mut v: Vec<String> = n.keys().cloned().collect();
            v.sort();
            v
        })
        .unwrap_or_default();
    CRow {
        id: c.id.unwrap_or_default(),
        name,
        image: c.image.unwrap_or_default(),
        image_id: c.image_id.unwrap_or_default(),
        state,
        status,
        health,
        ports: ports.join(", "),
        published,
        stack,
        created: c.created.unwrap_or(0),
        mounts,
        networks,
        tunnel: labels.get(crate::tunnel::LABEL).map(|t| crate::model::TunnelRef {
            target: t.clone(),
            target_id: labels.get("dockyard.tunnel.target_id").cloned().unwrap_or_default(),
            expires: labels.get("dockyard.tunnel.expires").and_then(|e| e.parse().ok()).unwrap_or(0),
        }),
    }
}

#[cfg(test)]
mod tests {
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
}
