//! Pestañas de imágenes, volúmenes y catálogo.

use super::*;

// ───────────────────────────── Imágenes / Volúmenes / Catálogo ─────────────────────────────

pub(super) fn draw_images(f: &mut Frame, area: Rect, app: &mut App) {
    let vis = app.visible_images();
    let total: u64 = app.images.iter().map(|i| i.size).sum();
    let rows: Vec<Row> = vis
        .iter()
        .map(|i| {
            let tag = if i.tags.is_empty() { "<sin etiqueta>".to_string() } else { i.tags.join(", ") };
            let used = if i.used_by.is_empty() { Span::styled("—", dim()) } else { Span::styled(i.used_by.join(", "), Style::default().fg(OKC)) };
            Row::new(vec![
                Cell::from(Span::styled(tag, if i.tags.is_empty() { dim() } else { bold() })),
                Cell::from(Span::styled(short_id(&i.id).to_string(), dim())),
                Cell::from(format!("{:>7}", human_bytes(i.size))),
                Cell::from(format!("hace {}", human_age(now_secs() - i.created))),
                Cell::from(used),
            ])
        })
        .collect();
    let mut title = vec![Span::styled(" Imágenes ", Style::default().fg(ACC)), Span::styled(format!("({}) · {} en total", vis.len(), human_bytes(total)), dim())];
    if !app.i_filter.is_empty() {
        title.push(Span::styled(format!(" · filtro: «{}»", app.i_filter), Style::default().fg(WARNC)));
    }
    title.push(Span::raw(" "));
    let table = Table::new(
        rows,
        [Constraint::Min(30), Constraint::Length(13), Constraint::Length(8), Constraint::Length(12), Constraint::Min(16)],
    )
    .header(Row::new(["IMAGEN", "ID", "TAMAÑO", "CREADA", "EN USO POR"]).style(dim()))
    .block(block(Line::from(title)))
    .row_highlight_style(sel_style());
    f.render_stateful_widget(table, area, &mut app.i_sel.ts);
}

pub(super) fn draw_volumes(f: &mut Frame, area: Rect, app: &mut App) {
    let detail_h = if area.height >= 16 { 4 } else { 0 };
    let [list, detail] = Layout::vertical([Constraint::Min(4), Constraint::Length(detail_h)]).areas(area);
    let vis = app.visible_volumes();
    let orphans = app.volumes.iter().filter(|v| v.used_by.is_empty()).count();
    let rows: Vec<Row> = vis
        .iter()
        .map(|v| {
            let used = if v.used_by.is_empty() { Span::styled("huérfano", Style::default().fg(WARNC)) } else { Span::styled(v.used_by.join(", "), Style::default().fg(OKC)) };
            let anon = v.name.len() == 64 && v.name.chars().all(|c| c.is_ascii_hexdigit());
            let size = match app.usage.as_ref().and_then(|u| u.vol_sizes.get(&v.name)) {
                Some(b) => Span::raw(format!("{:>7}", human_bytes(*b))),
                None => Span::styled("      …", dim()),
            };
            Row::new(vec![
                Cell::from(Span::styled(if anon { format!("{}… (anónimo)", &v.name[..12]) } else { v.name.clone() }, if anon { dim() } else { bold() })),
                Cell::from(size),
                Cell::from(Span::styled(v.driver.clone(), dim())),
                Cell::from(used),
            ])
        })
        .collect();
    let mut title = vec![Span::styled(" Volúmenes ", Style::default().fg(ACC)), Span::styled(format!("({}) · {orphans} huérfanos", vis.len()), dim())];
    if !app.v_filter.is_empty() {
        title.push(Span::styled(format!(" · filtro: «{}»", app.v_filter), Style::default().fg(WARNC)));
    }
    title.push(Span::raw(" "));
    let table = Table::new(rows, [Constraint::Min(30), Constraint::Length(8), Constraint::Length(9), Constraint::Min(20)])
        .header(Row::new(["NOMBRE", "TAMAÑO", "DRIVER", "EN USO POR"]).style(dim()))
        .block(block(Line::from(title)))
        .row_highlight_style(sel_style());
    f.render_stateful_widget(table, list, &mut app.v_sel.ts);
    if detail_h > 0 {
        let blk = block(Span::styled(" Detalle ", Style::default().fg(ACC)));
        let inner = blk.inner(detail);
        f.render_widget(blk, detail);
        if let Some(v) = app.cur_volume() {
            let lines = vec![
                kv("ruta", v.mountpoint.clone()),
                kv("respaldos", app.cfg.backups_dir().display().to_string()),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

pub(super) fn draw_catalog(f: &mut Frame, area: Rect, app: &mut App) {
    let [list, detail] = if area.width >= 100 {
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(100), Constraint::Length(0)]).areas(area)
    };
    let vis = app.visible_templates();
    let mut last_cat = String::new();
    let rows: Vec<Row> = vis
        .iter()
        .map(|t| {
            let cat = if t.category == last_cat { String::new() } else { t.category.clone() };
            last_cat = t.category.clone();
            let n = t.services.len();
            Row::new(vec![
                Cell::from(Span::styled(cat, Style::default().fg(ACC))),
                Cell::from(Span::styled(t.name.clone(), bold())),
                Cell::from(Span::styled(if n > 1 { format!("{n} servicios") } else { String::new() }, dim())),
            ])
        })
        .collect();
    let mut title = vec![Span::styled(" Catálogo ", Style::default().fg(ACC)), Span::styled(format!("({})", vis.len()), dim())];
    if !app.cat_filter.is_empty() {
        title.push(Span::styled(format!(" · búsqueda: «{}»", app.cat_filter), Style::default().fg(WARNC)));
    }
    title.push(Span::raw(" "));
    let table = Table::new(rows, [Constraint::Length(22), Constraint::Min(20), Constraint::Length(12)])
        .block(block(Line::from(title)))
        .row_highlight_style(sel_style());
    f.render_stateful_widget(table, list, &mut app.cat_sel.ts);

    if detail.width > 0 {
        let blk = block(Span::styled(" Plantilla ", Style::default().fg(ACC)));
        let inner = blk.inner(detail);
        f.render_widget(blk, detail);
        if let Some(t) = app.cur_template() {
            let mut lines: Vec<Line> = vec![
                Line::from(Span::styled(t.name.clone(), bold().fg(ACC))),
                Line::from(""),
                Line::from(t.description.clone()),
                Line::from(""),
            ];
            for s in &t.services {
                lines.push(Line::from(vec![
                    Span::styled(format!("▸ {:<7}", s.role), Style::default().fg(OKC)),
                    Span::raw(if s.image.is_empty() { "(tú eliges la imagen)".to_string() } else { s.image.clone() }),
                ]));
                if !s.ports.is_empty() {
                    lines.push(Line::from(vec![Span::styled("    puertos ", dim()), Span::raw(s.ports.join("  "))]));
                }
                if !s.volumes.is_empty() {
                    lines.push(Line::from(vec![Span::styled("    datos   ", dim()), Span::raw(s.volumes.iter().map(|v| v.replace("{{name}}", "‹nombre›")).collect::<Vec<_>>().join("  "))]));
                }
            }
            if !t.note.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(format!("ⓘ {}", t.note), Style::default().fg(WARNC))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Enter para instalar · podrás editar todo antes de crear", dim())));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}

