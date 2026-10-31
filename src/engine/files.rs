//! API de archivos de los contenedores y volúmenes, y el flujo de eventos del daemon.

use super::*;

impl Engine {
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
