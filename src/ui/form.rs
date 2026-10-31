//! Formulario de instalación y edición.

use super::*;

// ───────────────────────────── Formulario ─────────────────────────────

pub(super) fn draw_form(f: &mut Frame, area: Rect, fm: &Form, _app: &App) {
    let title = if fm.is_edit() {
        format!(" {} — se recreará el contenedor conservando sus datos ", fm.plan.template_name)
    } else {
        format!(" Instalar · {} → «{}» ", fm.plan.template_name, fm.plan.name)
    };
    let blk = block(Span::styled(truncate(&title, area.width.saturating_sub(4) as usize), Style::default().fg(ACC).add_modifier(Modifier::BOLD)));
    let inner = blk.inner(area);
    f.render_widget(blk, area);

    let problems = fm.problems();
    let prob_h = (problems.len() as u16).min(5) + if fm.msg.is_some() { 1 } else { 0 };
    let [list, probs] = Layout::vertical([Constraint::Min(3), Constraint::Length(prob_h)]).areas(inner);

    let w = list.width as usize;
    let label_w = 13usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    for (i, r) in fm.rows.iter().enumerate() {
        let (label, value, faint) = fm.row_text(*r);
        let selected = i == fm.sel;
        if selected {
            sel_line = lines.len();
        }
        let marker = if selected { Span::styled("▶ ", Style::default().fg(ACC)) } else { Span::raw("  ") };
        match r {
            FRow::Header(_) => {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::styled("── ", dim()),
                    Span::styled(label, Style::default().fg(OKC).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" → {value} "), bold()),
                    Span::styled("─".repeat(w.saturating_sub(12 + value.chars().count())), dim()),
                ]));
            }
            _ => {
                let mut spans = vec![marker, Span::styled(format!("{label:<label_w$}"), dim())];
                if let (Some((inp, er)), true) = (&fm.edit, selected) {
                    let _ = er;
                    spans.extend(inp.spans(w.saturating_sub(label_w + 4), Style::default().fg(Color::White)));
                    spans.push(Span::raw(""));
                } else {
                    let mut vstyle = if faint { dim() } else { Style::default() };
                    let mut warn = false;
                    if let FRow::Port(i, p) = r {
                        if fm.port_has_issue(&fm.plan.services[*i].ports[*p]) {
                            vstyle = Style::default().fg(ERRC);
                            warn = true;
                        }
                    }
                    if selected {
                        vstyle = vstyle.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(truncate(&value, w.saturating_sub(label_w + 4)), vstyle));
                    if warn {
                        spans.push(Span::styled("  ⚠ en uso", Style::default().fg(ERRC)));
                    }
                }
                let mut line = Line::from(spans);
                if selected && fm.edit.is_none() {
                    line = line.style(Style::default().bg(Color::Indexed(238)));
                }
                lines.push(line);
            }
        }
    }
    let h = list.height as usize;
    let scroll = if sel_line + 2 >= h { (sel_line + 3).saturating_sub(h) } else { 0 };
    let shown: Vec<Line> = lines.iter().skip(scroll).take(h).cloned().collect();
    f.render_widget(Paragraph::new(shown), list);
    if lines.len() > h {
        let mut sb = ScrollbarState::new(lines.len().saturating_sub(h)).position(scroll);
        f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None), list, &mut sb);
    }

    let mut pl: Vec<Line> = Vec::new();
    if let Some((m, l)) = &fm.msg {
        pl.push(Line::from(Span::styled(format!(" {m}"), Style::default().fg(lvl_color(*l)).add_modifier(Modifier::BOLD))));
    }
    for (m, l) in problems.iter().take(5) {
        let icon = if *l == Level::Err { "✘" } else { "!" };
        pl.push(Line::from(Span::styled(format!(" {icon} {}", truncate(m, w.saturating_sub(4))), Style::default().fg(lvl_color(*l)))));
    }
    f.render_widget(Paragraph::new(pl), probs);

    // cursor del campo en edición: ya está dibujado con REVERSED
    let _ = Text::default();
}

