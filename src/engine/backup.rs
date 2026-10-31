//! Respaldos y restauración: volúmenes, volcados SQL, ayudantes efímeros y clonado.

use super::*;

impl Engine {
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
    pub(super) async fn wait_exit(&self, name: &str) -> Result<i64> {
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


    /// ¿Existe el volumen? No lo crea: montarlo para respaldarlo sí lo crearía vacío.
    pub async fn volume_exists(&self, name: &str) -> Result<bool> {
        match self.d.inspect_volume(name).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}
