//! Avisos, preguntas y ventanas de entrada.

use super::*;

// ───────────────────────────── Ventanas emergentes ─────────────────────────────

pub(super) fn draw_prompt(f: &mut Frame, area: Rect, p: &Prompt) {
    let r = centered(area, area.width.min(80), 6);
    f.render_widget(Clear, r);
    let blk = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACC))
        .title(Span::styled(format!(" {} ", p.title), Style::default().fg(ACC).add_modifier(Modifier::BOLD)));
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let [a, b, c] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    let mut spans = vec![Span::styled("› ", Style::default().fg(ACC))];
    // en «Traer desde Git» el token que se pegue dentro de la URL no se deja a la vista
    let shown = if matches!(p.action, crate::app::PromptAction::Git) {
        crate::input::LineInput { buf: crate::ext::mask_auth(&p.input.text()).chars().collect(), cur: p.input.cur }
    } else {
        p.input.clone()
    };
    spans.extend(shown.spans(a.width.saturating_sub(3) as usize, Style::default()));
    f.render_widget(Paragraph::new(Line::from(spans)), a);
    let _ = b;
    f.render_widget(Paragraph::new(Span::styled(truncate(&p.hint, c.width as usize), dim())), c);
}

pub(super) fn draw_confirm(f: &mut Frame, area: Rect, c: &Confirm) {
    let h = c.lines.len() as u16 + 4;
    let r = centered(area, area.width.min(84), h);
    f.render_widget(Clear, r);
    let col = if c.danger { ERRC } else { ACC };
    let blk = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(col))
        .title(Span::styled(format!(" {} ", c.title), Style::default().fg(col).add_modifier(Modifier::BOLD)));
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    let mut lines: Vec<Line> = c.lines.iter().map(|l| Line::from(l.clone())).collect();
    lines.push(Line::from(""));
    let mut buttons = vec![Span::styled(" s ", Style::default().fg(Color::Black).bg(col).add_modifier(Modifier::BOLD)), Span::raw(" Sí    ")];
    if let Some((k, d)) = &c.alt {
        buttons.push(Span::styled(format!(" {k} "), Style::default().fg(Color::Black).bg(WARNC).add_modifier(Modifier::BOLD)));
        buttons.push(Span::raw(format!(" {d}    ")));
    }
    buttons.push(Span::styled(" n ", Style::default().fg(Color::Black).bg(DIM).add_modifier(Modifier::BOLD)));
    buttons.push(Span::raw(" No"));
    lines.push(Line::from(buttons));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

