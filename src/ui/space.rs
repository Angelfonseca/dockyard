//! Pestaña de espacio: uso de disco y limpieza.

use super::*;

// ───────────────────────────── Espacio ─────────────────────────────

pub(super) fn draw_space(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(u) = app.usage.clone() else {
        let blk = block(Span::styled(" Espacio ", Style::default().fg(ACC)));
        let inner = blk.inner(area);
        f.render_widget(blk, area);
        f.render_widget(Paragraph::new(Span::styled(format!(" {} Calculando uso de disco (con volúmenes grandes puede tardar)…", spinner(app)), dim())), inner);
        return;
    };
    let [top, bottom] = Layout::vertical([Constraint::Length(9), Constraint::Min(3)]).areas(area);
    let total: u64 = [&u.images, &u.containers, &u.volumes, &u.build].iter().map(|x| x.size).sum();
    let recl: u64 = [&u.images, &u.containers, &u.build].iter().map(|x| x.reclaimable).sum();
    let mut title = vec![
        Span::styled(" Espacio de Docker ", Style::default().fg(ACC)),
        Span::styled(format!("· {} en total · {} recuperables sin tocar datos ", human_bytes(total), human_bytes(recl)), dim()),
    ];
    if let Some((root, used, tot)) = &u.root {
        title.push(Span::styled(format!("· disco {root}: {}/{} ", human_bytes(*used), human_bytes(*tot)), dim()));
    }
    let max = [&u.images, &u.containers, &u.volumes, &u.build].iter().map(|x| x.size).max().unwrap_or(1).max(1);
    let row = |key: &'static str, name: &'static str, x: &Usage, note: &'static str| {
        let (size, rec) = if x.known { (human_bytes(x.size), human_bytes(x.reclaimable)) } else { ("n/d".into(), "n/d".into()) };
        Row::new(vec![
            Cell::from(Span::styled(format!(" {key} "), Style::default().fg(Color::Black).bg(ACC).add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled(name, bold())),
            Cell::from(format!("{:>4} / {:<4}", x.active, x.count)),
            Cell::from(format!("{size:>8}")),
            Cell::from(Line::from(bar(x.size as f64 / max as f64, 12, ACC))),
            Cell::from(Span::styled(format!("{rec:>8}"), Style::default().fg(if x.reclaimable > 0 { WARNC } else { DIM }))),
            Cell::from(Span::styled(note, dim())),
        ])
    };
    let rows = vec![
        row("i", "Imágenes", &u.images, "sin uso: se vuelven a descargar"),
        row("c", "Contenedores", &u.containers, "detenidos (sus volúmenes se conservan)"),
        row("v", "Volúmenes", &u.volumes, "huérfanos: ¡borra datos!"),
        row("b", "Caché de build", &u.build, "se regenera al construir"),
    ];
    let t = Table::new(
        rows,
        [Constraint::Length(3), Constraint::Length(15), Constraint::Length(11), Constraint::Length(9), Constraint::Length(13), Constraint::Length(10), Constraint::Min(10)],
    )
    .header(Row::new(["", "TIPO", "USO / TOTAL", "TAMAÑO", "", "RECUPERA", ""]).style(dim()))
    .block(block(Line::from(title)));
    f.render_widget(t, top);

    // volúmenes más grandes
    let mut vols: Vec<(&String, &u64)> = u.vol_sizes.iter().collect();
    vols.sort_by(|a, b| b.1.cmp(a.1));
    let vmax = vols.first().map(|v| *v.1).unwrap_or(1).max(1);
    let rows: Vec<Row> = vols
        .iter()
        .take(bottom.height.saturating_sub(3) as usize)
        .map(|(n, sz)| {
            let used: Vec<String> = app.volumes.iter().find(|v| &v.name == *n).map(|v| v.used_by.clone()).unwrap_or_default();
            Row::new(vec![
                Cell::from(format!("{:>8}", human_bytes(**sz))),
                Cell::from(Line::from(bar(**sz as f64 / vmax as f64, 16, Color::Magenta))),
                Cell::from(Span::styled(n.to_string(), bold())),
                Cell::from(if used.is_empty() { Span::styled("huérfano", Style::default().fg(WARNC)) } else { Span::styled(used.join(", "), Style::default().fg(OKC)) }),
            ])
        })
        .collect();
    let t = Table::new(rows, [Constraint::Length(9), Constraint::Length(17), Constraint::Min(24), Constraint::Min(16)])
        .block(block(Span::styled(" Volúmenes más grandes ", Style::default().fg(ACC))));
    f.render_widget(t, bottom);
}

