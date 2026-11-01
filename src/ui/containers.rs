//! Pestaña de contenedores y su detalle.

use super::*;

// ───────────────────────────── Contenedores ─────────────────────────────

pub(super) fn draw_containers(f: &mut Frame, area: Rect, app: &mut App) {
    let detail_h = if area.height >= 22 { 9 } else { 0 };
    let [list, detail] = Layout::vertical([Constraint::Min(5), Constraint::Length(detail_h)]).areas(area);

    let w = list.width;
    let show_image = w >= 100;
    let show_net = w >= 128;
    let show_ports = w >= 84;
    let host_mem = app.snap.host.mem_total.max(1);

    let mut header = vec!["", "NOMBRE", "ESTADO", "CPU", "MEMORIA"];
    let mut widths = vec![Constraint::Length(2), Constraint::Min(16), Constraint::Length(24), Constraint::Length(7), Constraint::Length(15)];
    if show_net {
        header.push("RED ↓ / ↑");
        widths.push(Constraint::Length(19));
    }
    if show_image {
        header.push("IMAGEN");
        widths.push(Constraint::Min(18));
    }
    if show_ports {
        header.push("PUERTOS");
        widths.push(Constraint::Min(14));
    }

    let vis = app.visible_containers();
    let rows: Vec<Row> = vis
        .iter()
        .map(|c| {
            let st = app.snap.stats.get(&c.id);
            let (cpu, mem) = match st {
                Some(s) => {
                    let lim = if s.mem_limit > 0 { s.mem_limit } else { host_mem };
                    let mp = s.mem as f64 * 100.0 / lim.max(1) as f64;
                    (
                        Cell::from(Span::styled(format!("{:>5.1}%", s.cpu), Style::default().fg(pct_color(s.cpu.min(100.0))))),
                        Cell::from(Span::styled(
                            format!("{:>6} {:>4.0}%", human_bytes(s.mem), mp),
                            Style::default().fg(pct_color(mp)),
                        )),
                    )
                }
                None => (Cell::from(Span::styled("    —", dim())), Cell::from(Span::styled("    —", dim()))),
            };
            let status_col = match (c.state, c.health) {
                (_, Some(Health::Unhealthy)) => ERRC,
                (St::Running, _) => Color::Reset,
                (St::Paused, _) | (St::Restarting, _) => WARNC,
                (St::Dead, _) => ERRC,
                _ => DIM,
            };
            let name = match &c.stack {
                Some(s) if s != &c.name => Line::from(vec![Span::styled(c.name.clone(), bold()), Span::styled(format!("  ‹{s}›"), dim())]),
                _ => Line::from(Span::styled(c.name.clone(), bold())),
            };
            let mut cells = vec![
                Cell::from(state_dot(c)),
                Cell::from(name),
                Cell::from(Span::styled(truncate(&es_status(&c.status), 23), Style::default().fg(status_col))),
                cpu,
                mem,
            ];
            if show_net {
                cells.push(Cell::from(match st {
                    Some(s) => Line::from(vec![
                        Span::styled(format!("{:>8}", human_rate(s.rx_rate)), Style::default().fg(OKC)),
                        Span::styled(" ", dim()),
                        Span::styled(format!("{:>8}", human_rate(s.tx_rate)), Style::default().fg(WARNC)),
                    ]),
                    None => Line::from(""),
                }));
            }
            if show_image {
                cells.push(Cell::from(Span::styled(c.image.clone(), dim())));
            }
            if show_ports {
                cells.push(Cell::from(Span::styled(c.ports.clone(), Style::default().fg(ACC))));
            }
            Row::new(cells)
        })
        .collect();
    let count = vis.len();
    let mut title = vec![Span::styled(" Contenedores ", Style::default().fg(ACC)), Span::styled(format!("({count}) · orden: {}", app.sort.label()), dim())];
    if !app.show_all {
        title.push(Span::styled(" · solo activos", Style::default().fg(WARNC)));
    }
    if !app.c_filter.is_empty() {
        title.push(Span::styled(format!(" · filtro: «{}»", app.c_filter), Style::default().fg(WARNC)));
    }
    title.push(Span::raw(" "));
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .block(block(Line::from(title)))
        .row_highlight_style(sel_style());
    f.render_stateful_widget(table, list, &mut app.c_sel.ts);
    if count == 0 {
        let msg = if app.snap.containers.is_empty() { "No hay contenedores. Pulsa n para abrir el catálogo e instalar algo." } else { "Ningún contenedor coincide con el filtro." };
        let r = Rect { x: list.x + 2, y: list.y + 2, width: list.width.saturating_sub(4), height: 1 };
        f.render_widget(Paragraph::new(Span::styled(msg, dim())), r);
    }

    if detail_h > 0 {
        draw_container_detail(f, detail, app);
    }
}

pub(super) fn kv(k: &str, v: String) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k:<9}"), dim()), Span::raw(v)])
}

pub(super) fn draw_container_detail(f: &mut Frame, area: Rect, app: &App) {
    let blk = block(Span::styled(" Detalle ", Style::default().fg(ACC)));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    let Some(c) = app.cur_container() else { return };
    let [l, r] = Layout::horizontal([Constraint::Min(30), Constraint::Length(inner.width.min(60) * 40 / 100 + 20)]).areas(inner);
    let age = human_age(now_secs() - c.created);
    let mut lines = vec![
        kv("nombre", c.name.clone()),
        kv("imagen", c.image.clone()),
        kv("id", short_id(&c.id).to_string()),
        kv("creado", format!("hace {age}")),
        kv("estado", es_status(&c.status)),
    ];
    if let Some(s) = &c.stack {
        lines.push(kv("stack", s.clone()));
    }
    if !c.networks.is_empty() {
        lines.push(kv("redes", c.networks.join(", ")));
    }
    if !c.mounts.is_empty() {
        lines.push(kv("montajes", truncate(&c.mounts.join(", "), l.width.saturating_sub(10) as usize)));
    }
    f.render_widget(Paragraph::new(lines), l);

    if let Some(h) = app.chist.get(&c.id) {
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1), Constraint::Min(1)]).split(r);
        let st = app.snap.stats.get(&c.id).cloned().unwrap_or_default();
        f.render_widget(Paragraph::new(Span::styled(format!("CPU {:.1}%", st.cpu), dim())), rows[0]);
        f.render_widget(spark(&h.0, 100.max(h.0.iter().copied().max().unwrap_or(0)), ACC, rows[1].width as usize), rows[1]);
        f.render_widget(Paragraph::new(Span::styled(format!("Memoria {}", human_bytes(st.mem)), dim())), rows[2]);
        f.render_widget(spark(&h.1, 100, Color::Magenta, rows[3].width as usize), rows[3]);
    }
}

