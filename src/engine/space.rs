//! Uso de disco (`system df`) y credenciales de los contenedores.

use super::*;

impl Engine {
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

}
