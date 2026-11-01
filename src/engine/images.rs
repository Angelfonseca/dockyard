//! Imágenes, limpieza por lotes y volúmenes huérfanos.

use super::*;

impl Engine {
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

}
