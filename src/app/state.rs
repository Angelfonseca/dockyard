//! Estado de la vista: totales, listas visibles, selecciones y el historial de las gráficas.

use super::*;

impl App {
    // ───────────────────────────── Utilidades de estado ─────────────────────────────

    pub fn toast(&mut self, msg: impl Into<String>, lvl: Level) {
        self.toast = Some((msg.into(), lvl, Instant::now()));
    }

    pub fn refresh_now(&self) {
        self.shared.notify.notify_one();
    }

    pub(super) fn set_extra(&self) {
        let want = matches!(self.tab, Tab::Images | Tab::Volumes | Tab::Containers | Tab::Space) || matches!(self.mode, Mode::Form(_));
        self.shared.want_extra.store(want, Ordering::Relaxed);
        self.shared.want_usage.store(matches!(self.tab, Tab::Volumes | Tab::Space), Ordering::Relaxed);
    }

    pub fn goto(&mut self, t: Tab) {
        self.tab = t;
        self.set_extra();
        if matches!(t, Tab::Images | Tab::Volumes | Tab::Space) {
            self.refresh_now();
        }
    }

    /// Recalcular el uso de disco ya (tras borrar algo).
    pub(super) fn refresh_usage(&self) {
        self.shared.usage_now.notify_one();
    }

    pub(super) fn push_hist(q: &mut VecDeque<u64>, v: u64) {
        if q.len() >= HIST {
            q.pop_front();
        }
        q.push_back(v);
    }

    pub fn apply_snapshot(&mut self, mut s: Snapshot) {
        if let Some(i) = s.images.take() {
            self.images = i;
        }
        if let Some(v) = s.volumes.take() {
            self.volumes = v;
        }
        let h = &s.host;
        Self::push_hist(&mut self.hist.cpu, h.cpu.round().clamp(0.0, 100.0) as u64);
        let mp = if h.mem_total > 0 { h.mem_used * 100 / h.mem_total } else { 0 };
        Self::push_hist(&mut self.hist.mem, mp);
        Self::push_hist(&mut self.hist.rx, h.net_rx_rate as u64);
        Self::push_hist(&mut self.hist.tx, h.net_tx_rate as u64);
        for c in &s.containers {
            if let Some(st) = s.stats.get(&c.id) {
                let e = self.chist.entry(c.id.clone()).or_default();
                Self::push_hist(&mut e.0, st.cpu.round().max(0.0) as u64);
                let lim = if st.mem_limit > 0 { st.mem_limit } else { s.host.mem_total.max(1) };
                Self::push_hist(&mut e.1, st.mem * 100 / lim.max(1));
            }
        }
        let ids: HashSet<&String> = s.containers.iter().map(|c| &c.id).collect();
        self.chist.retain(|k, _| ids.contains(k));
        self.snap = s;
        self.connected = true;
        self.conn_err = None;
    }

    pub fn visible_containers(&self) -> Vec<&CRow> {
        let f = self.c_filter.to_lowercase();
        let mut v: Vec<&CRow> = self
            .snap
            .containers
            .iter()
            .filter(|c| self.show_all || c.is_up())
            .filter(|c| {
                f.is_empty()
                    || c.name.to_lowercase().contains(&f)
                    || c.image.to_lowercase().contains(&f)
                    || c.stack.as_deref().map(|s| s.to_lowercase().contains(&f)).unwrap_or(false)
                    || c.state.label().contains(&f)
            })
            .collect();
        let stats = &self.snap.stats;
        match self.sort {
            Sort::Name => v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            Sort::Cpu => v.sort_by(|a, b| {
                let ca = stats.get(&a.id).map(|s| s.cpu).unwrap_or(-1.0);
                let cb = stats.get(&b.id).map(|s| s.cpu).unwrap_or(-1.0);
                cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Mem => v.sort_by(|a, b| {
                let ma = stats.get(&a.id).map(|s| s.mem).unwrap_or(0);
                let mb = stats.get(&b.id).map(|s| s.mem).unwrap_or(0);
                mb.cmp(&ma)
            }),
            Sort::State => v.sort_by(|a, b| {
                (!a.is_up(), a.name.to_lowercase()).cmp(&(!b.is_up(), b.name.to_lowercase()))
            }),
        }
        v
    }

    pub fn visible_images(&self) -> Vec<&IRow> {
        let f = self.i_filter.to_lowercase();
        self.images
            .iter()
            .filter(|i| f.is_empty() || i.tags.iter().any(|t| t.to_lowercase().contains(&f)) || i.id.contains(&f))
            .collect()
    }

    pub fn visible_volumes(&self) -> Vec<&VRow> {
        let f = self.v_filter.to_lowercase();
        self.volumes.iter().filter(|v| f.is_empty() || v.name.to_lowercase().contains(&f)).collect()
    }

    pub fn visible_templates(&self) -> Vec<&Template> {
        let f = self.cat_filter.to_lowercase();
        self.catalog
            .templates
            .iter()
            .filter(|t| {
                f.is_empty()
                    || t.id.contains(&f)
                    || t.name.to_lowercase().contains(&f)
                    || t.category.to_lowercase().contains(&f)
                    || t.description.to_lowercase().contains(&f)
            })
            .collect()
    }

    pub fn sync_selections(&mut self) {
        let ck: Vec<String> = self.visible_containers().iter().map(|c| c.id.clone()).collect();
        self.c_sel.sync(&ck);
        let ik: Vec<String> = self.visible_images().iter().map(|c| c.id.clone()).collect();
        self.i_sel.sync(&ik);
        let vk: Vec<String> = self.visible_volumes().iter().map(|c| c.name.clone()).collect();
        self.v_sel.sync(&vk);
        let tk: Vec<String> = self.visible_templates().iter().map(|c| c.id.clone()).collect();
        self.cat_sel.sync(&tk);
    }

    pub fn cur_container(&self) -> Option<&CRow> {
        let k = self.c_sel.key.as_ref()?;
        self.snap.containers.iter().find(|c| &c.id == k)
    }

    pub fn cur_image(&self) -> Option<&IRow> {
        let k = self.i_sel.key.as_ref()?;
        self.images.iter().find(|c| &c.id == k)
    }

    pub fn cur_volume(&self) -> Option<&VRow> {
        let k = self.v_sel.key.as_ref()?;
        self.volumes.iter().find(|c| &c.name == k)
    }

    pub fn cur_template(&self) -> Option<&Template> {
        let k = self.cat_sel.key.as_ref()?;
        self.catalog.get(k)
    }

}
