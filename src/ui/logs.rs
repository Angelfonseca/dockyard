//! Visor de logs y de texto.

use super::*;

// ───────────────────────────── Logs y texto ─────────────────────────────

pub(super) fn log_style(l: &str) -> (String, Style) {
    let (txt, err) = match l.strip_prefix('\u{1}') {
        Some(r) => (r.to_string(), true),
        None => (l.to_string(), false),
    };
    let low = txt.to_lowercase();
    let style = if low.contains("error") || low.contains("fatal") || low.contains("panic") || low.contains("exception") || low.contains("critical") {
        Style::default().fg(ERRC)
    } else if low.contains("warn") {
        Style::default().fg(WARNC)
    } else if err {
        Style::default().fg(Color::LightRed)
    } else {
        Style::default()
    };
    (txt, style)
}

pub(super) fn draw_logs(f: &mut Frame, area: Rect, lv: &LogView) {
    let n_filtered = filtered_len(lv);
    let head = match lv.kind {
        LogKind::Container => format!(" logs · {} ", lv.name),
        LogKind::Command => format!(" {} ", truncate(&lv.name, (area.width as usize).saturating_sub(40).max(10))),
    };
    let mut title = vec![Span::styled(head, Style::default().fg(ACC).add_modifier(Modifier::BOLD)), Span::styled(format!("{} líneas ", n_filtered), dim())];
    if lv.kind == LogKind::Command && lv.ended.is_none() {
        title.push(Span::styled("⏵ en curso ", Style::default().fg(ACC)));
    }
    title.push(if lv.follow { Span::styled("▼ siguiendo ", Style::default().fg(OKC)) } else { Span::styled("‖ en pausa ", Style::default().fg(WARNC)) });
    if !lv.filter.is_empty() {
        title.push(Span::styled(format!("· filtro «{}» ", lv.filter), Style::default().fg(WARNC)));
    }
    if lv.timestamps {
        title.push(Span::styled("· hora ", dim()));
    }
    if let (Some(e), LogKind::Container) = (&lv.ended, lv.kind) {
        title.push(Span::styled(format!("· {} ", truncate(e, 50)), Style::default().fg(ERRC)));
    }
    let blk = block(Line::from(title));
    let inner = blk.inner(area);
    f.render_widget(blk, area);

    // tareas: el resultado final va en una línea propia (puede ser largo)
    let end_line = match (&lv.ended, lv.kind) {
        (Some(e), LogKind::Command) => Some(Line::from(Span::styled(
            truncate(&e.replace('\n', " · "), inner.width as usize),
            Style::default().fg(if lv.ended_ok { OKC } else { ERRC }).add_modifier(Modifier::BOLD),
        ))),
        _ => None,
    };
    let filter_h = if lv.filter_input.is_some() { 1 } else { 0 };
    let end_h = if end_line.is_some() { 1 } else { 0 };
    let [text_area, end_area, filter_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(end_h), Constraint::Length(filter_h)]).areas(inner);
    if let Some(l) = end_line {
        f.render_widget(Paragraph::new(l), end_area);
    }
    let h = text_area.height as usize;
    let w = text_area.width.saturating_sub(1) as usize;

    let flt = lv.filter.to_lowercase();
    let all: Vec<&String> = if flt.is_empty() { lv.lines.iter().collect() } else { lv.lines.iter().filter(|l| l.to_lowercase().contains(&flt)).collect() };
    let max = all.len().saturating_sub(1);
    let bottom = if lv.follow { max } else { lv.scroll.max(h.saturating_sub(1)).min(max) };
    let start = (bottom + 1).saturating_sub(h);
    let window = if all.is_empty() { &[][..] } else { &all[start..=bottom] };

    let mut out: Vec<Line> = Vec::new();
    for l in window {
        let (txt, style) = log_style(l);
        if lv.wrap {
            let chars: Vec<char> = txt.chars().collect();
            if chars.is_empty() {
                out.push(Line::from(""));
            }
            for ch in chars.chunks(w.max(1)) {
                out.push(Line::from(Span::styled(ch.iter().collect::<String>(), style)));
            }
        } else {
            let s: String = txt.chars().skip(lv.hscroll).take(w).collect();
            out.push(Line::from(Span::styled(s, style)));
        }
    }
    if out.len() > h {
        out.drain(..out.len() - h);
    }
    if out.is_empty() {
        let msg = match (lv.lines.is_empty(), lv.kind) {
            (true, LogKind::Container) => "esperando logs…",
            (true, LogKind::Command) => if lv.ended.is_some() { "(sin salida)" } else { "esperando salida…" },
            _ => "ninguna línea coincide con el filtro",
        };
        out.push(Line::from(Span::styled(msg, dim())));
    }
    f.render_widget(Paragraph::new(out), text_area);
    if all.len() > h {
        let mut sb = ScrollbarState::new(all.len().saturating_sub(h)).position(start);
        f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None), text_area, &mut sb);
    }
    if let Some(inp) = &lv.filter_input {
        let mut spans = vec![Span::styled("/ ", Style::default().fg(ACC))];
        spans.extend(inp.spans(filter_area.width.saturating_sub(3) as usize, Style::default()));
        f.render_widget(Paragraph::new(Line::from(spans)), filter_area);
    }
}

pub(super) fn draw_text(f: &mut Frame, area: Rect, tv: &TextView) {
    let blk = block(Span::styled(format!(" {} ", truncate(&tv.title, area.width.saturating_sub(6) as usize)), Style::default().fg(ACC).add_modifier(Modifier::BOLD)));
    let inner = blk.inner(area);
    f.render_widget(blk, area);
    let h = inner.height as usize;
    let w = inner.width.saturating_sub(1) as usize;
    let lines: Vec<Line> = tv
        .lines
        .iter()
        .skip(tv.scroll)
        .take(h)
        .map(|l| {
            let s: String = l.chars().skip(tv.hscroll).take(w).collect();
            let st = if s.trim_start().starts_with('✔') {
                Style::default().fg(OKC).add_modifier(Modifier::BOLD)
            } else if s.contains(" = ") && s.starts_with("  ") && !s.starts_with("  •") {
                Style::default().fg(WARNC)
            } else {
                Style::default()
            };
            Line::from(Span::styled(s, st))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    if tv.lines.len() > h {
        let mut sb = ScrollbarState::new(tv.lines.len().saturating_sub(h)).position(tv.scroll);
        f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None), inner, &mut sb);
    }
}

