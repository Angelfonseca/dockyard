//! Listados: contenedores, estadísticas, imágenes y volúmenes con su uso.

use super::*;

impl Engine {
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

}
