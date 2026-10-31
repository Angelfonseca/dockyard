//! Panel: recursos del servidor con gráficas y contenedores con problemas.

use super::*;

// ───────────────────────────── Panel ─────────────────────────────

pub(super) fn spark<'a>(data: &'a std::collections::VecDeque<u64>, max: u64, color: Color, w: usize) -> Sparkline<'a> {
    let skip = data.len().saturating_sub(w);
    Sparkline::default().data(data.iter().skip(skip).copied()).max(max.max(1)).style(Style::default().fg(color))
}

pub(super) fn draw_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let h = app.snap.host.clone();
    let top_h = if area.height >= 24 { 11 } else { 8 };
    let [top, bottom] = Layout::vertical([Constraint::Length(top_h), Constraint::Min(4)]).areas(area);
    let [a, b, c] = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)]).areas(top);

    // CPU
    {
        let title = format!(" CPU · {} hilos ", h.cores.len());
        let blk = block(Span::styled(title, Style::default().fg(ACC)));
        let inner = blk.inner(a);
        f.render_widget(blk, a);
        let bw = (inner.width as usize).saturating_sub(9).max(4);
        let mut lines = vec![
            Line::from(
                [vec![Span::styled(format!("{:>5.1}% ", h.cpu), Style::default().fg(pct_color(h.cpu as f64)).add_modifier(Modifier::BOLD))], bar(h.cpu as f64 / 100.0, bw, pct_color(h.cpu as f64))].concat(),
            ),
            Line::from(vec![
                Span::styled("carga ", dim()),
                Span::raw(format!("{:.2} {:.2} {:.2}", h.load.0, h.load.1, h.load.2)),
                Span::styled("  temp ", dim()),
                match h.temp_c {
                    Some(t) => Span::styled(format!("{t:.0}°C"), Style::default().fg(if t < 70.0 { OKC } else if t < 85.0 { WARNC } else { ERRC })),
                    None => Span::styled("n/d", dim()),
                },
            ]),
        ];
        let blocks = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        let mut cores = vec![Span::styled("núcleos ", dim())];
        for c in &h.cores {
            let i = ((c / 100.0) * 7.0).round().clamp(0.0, 7.0) as usize;
            cores.push(Span::styled(blocks[i], Style::default().fg(pct_color(*c as f64))));
        }
        lines.push(Line::from(cores));
        let tl = lines.len() as u16;
        f.render_widget(Paragraph::new(lines), Rect { height: tl.min(inner.height), ..inner });
        if inner.height > tl {
            let r = Rect { y: inner.y + tl, height: inner.height - tl, ..inner };
            f.render_widget(spark(&app.hist.cpu, 100, ACC, r.width as usize), r);
        }
    }
    // Memoria
    {
        let blk = block(Span::styled(" Memoria ", Style::default().fg(ACC)));
        let inner = blk.inner(b);
        f.render_widget(blk, b);
        let bw = (inner.width as usize).saturating_sub(24).max(4);
        let mp = if h.mem_total > 0 { h.mem_used as f64 * 100.0 / h.mem_total as f64 } else { 0.0 };
        let sp = if h.swap_total > 0 { h.swap_used as f64 * 100.0 / h.swap_total as f64 } else { 0.0 };
        let lines = vec![
            Line::from(
                [
                    vec![Span::styled("RAM  ", dim())],
                    bar(mp / 100.0, bw, pct_color(mp)),
                    vec![Span::raw(format!(" {}/{}", human_bytes(h.mem_used), human_bytes(h.mem_total)))],
                ]
                .concat(),
            ),
            Line::from(
                [
                    vec![Span::styled("Swap ", dim())],
                    bar(sp / 100.0, bw, pct_color(sp)),
                    vec![Span::raw(format!(" {}/{}", human_bytes(h.swap_used), human_bytes(h.swap_total)))],
                ]
                .concat(),
            ),
            Line::from(vec![Span::styled("activo ", dim()), Span::raw(human_uptime(h.uptime))]),
        ];
        let tl = lines.len() as u16;
        f.render_widget(Paragraph::new(lines), Rect { height: tl.min(inner.height), ..inner });
        if inner.height > tl {
            let r = Rect { y: inner.y + tl, height: inner.height - tl, ..inner };
            f.render_widget(spark(&app.hist.mem, 100, Color::Magenta, r.width as usize), r);
        }
    }
    // Disco y red
    {
        let blk = block(Span::styled(" Disco y red ", Style::default().fg(ACC)));
        let inner = blk.inner(c);
        f.render_widget(blk, c);
        let bw = (inner.width as usize).saturating_sub(28).max(4);
        let mut lines: Vec<Line> = Vec::new();
        for d in h.disks.iter().take(3) {
            let p = if d.total > 0 { d.used as f64 * 100.0 / d.total as f64 } else { 0.0 };
            lines.push(Line::from(
                [
                    vec![Span::styled(format!("{:<8}", truncate(&d.mount, 8)), dim())],
                    bar(p / 100.0, bw, pct_color(p)),
                    vec![Span::raw(format!(" {}/{}", human_bytes(d.used), human_bytes(d.total)))],
                ]
                .concat(),
            ));
        }
        lines.push(Line::from(vec![
            Span::styled("↓ ", Style::default().fg(OKC)),
            Span::raw(format!("{:<10}", human_rate(h.net_rx_rate))),
            Span::styled("↑ ", Style::default().fg(WARNC)),
            Span::raw(human_rate(h.net_tx_rate)),
        ]));
        let tl = lines.len() as u16;
        f.render_widget(Paragraph::new(lines), Rect { height: tl.min(inner.height), ..inner });
        if inner.height > tl {
            let r = Rect { y: inner.y + tl, height: inner.height - tl, ..inner };
            let mx = app.hist.rx.iter().chain(app.hist.tx.iter()).copied().max().unwrap_or(1).max(1024);
            f.render_widget(spark(&app.hist.rx, mx, OKC, r.width as usize), r);
        }
    }

    // Contenedores activos por CPU
    let running = app.snap.containers.iter().filter(|c| c.state == St::Running).count();
    let stopped = app.snap.containers.iter().filter(|c| !c.is_up()).count();
    let bad = app.snap.containers.iter().filter(|c| c.health == Some(Health::Unhealthy) || c.state == St::Dead).count();
    let title = Line::from(vec![
        Span::styled(" Contenedores ", Style::default().fg(ACC)),
        Span::styled(format!("· {running} activos · {stopped} detenidos"), dim()),
        if bad > 0 { Span::styled(format!(" · {bad} con problemas "), Style::default().fg(ERRC)) } else { Span::raw(" ") },
    ]);
    let mut rows_src: Vec<&CRow> = app.snap.containers.iter().filter(|c| c.state == St::Running).collect();
    rows_src.sort_by(|a, b| {
        let ca = app.snap.stats.get(&a.id).map(|s| s.cpu).unwrap_or(0.0);
        let cb = app.snap.stats.get(&b.id).map(|s| s.cpu).unwrap_or(0.0);
        cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
    });
    let hdr = Row::new(["", "NOMBRE", "CPU", "", "MEMORIA", "", "RED ↓", "RED ↑"]).style(dim());
    let rows: Vec<Row> = rows_src
        .iter()
        .map(|c| {
            let st = app.snap.stats.get(&c.id).cloned().unwrap_or_default();
            let lim = if st.mem_limit > 0 { st.mem_limit } else { h.mem_total.max(1) };
            let mp = st.mem as f64 * 100.0 / lim.max(1) as f64;
            let cpu_ratio = (st.cpu / 100.0 / (h.cores.len().max(1) as f64)).clamp(0.0, 1.0);
            Row::new(vec![
                Cell::from(state_dot(c)),
                Cell::from(Span::styled(c.name.clone(), bold())),
                Cell::from(Span::styled(format!("{:>5.1}%", st.cpu), Style::default().fg(pct_color(st.cpu.min(100.0))))),
                Cell::from(Line::from(bar(cpu_ratio.sqrt(), 8, pct_color(cpu_ratio.sqrt() * 100.0)))),
                Cell::from(format!("{:>6}", human_bytes(st.mem))),
                Cell::from(Line::from(bar(mp / 100.0, 8, pct_color(mp)))),
                Cell::from(Span::styled(human_rate(st.rx_rate), Style::default().fg(OKC))),
                Cell::from(Span::styled(human_rate(st.tx_rate), Style::default().fg(WARNC))),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(2),
        Constraint::Min(14),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    f.render_widget(Table::new(rows, widths).header(hdr).block(block(title)), bottom);
}

