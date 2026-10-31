//! Colores, estilos y utilidades de dibujo compartidas por todas las vistas.

use super::*;

pub(super) const ACC: Color = Color::Cyan;
pub(super) const OKC: Color = Color::Green;
pub(super) const WARNC: Color = Color::Yellow;
pub(super) const ERRC: Color = Color::Red;
pub(super) const DIM: Color = Color::DarkGray;

pub(super) fn lvl_color(l: Level) -> Color {
    match l {
        Level::Info => ACC,
        Level::Ok => OKC,
        Level::Warn => WARNC,
        Level::Err => ERRC,
    }
}

pub(super) fn pct_color(p: f64) -> Color {
    if p < 60.0 {
        OKC
    } else if p < 85.0 {
        WARNC
    } else {
        ERRC
    }
}

pub(super) fn dim() -> Style {
    Style::default().fg(DIM)
}

pub(super) fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub(super) fn sel_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

pub(super) fn bar(ratio: f64, w: usize, color: Color) -> Vec<Span<'static>> {
    let r = ratio.clamp(0.0, 1.0);
    let filled = ((r * w as f64).round() as usize).min(w);
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(w - filled), dim()),
    ]
}

pub(super) fn block<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title(title)
}

pub(super) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub(super) fn spinner(app: &App) -> &'static str {
    const F: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    F[(app.started.elapsed().as_millis() / 90) as usize % F.len()]
}

pub(super) fn es_status(s: &str) -> String {
    let mut t = s.to_string();
    for (a, b) in [
        ("Up ", "Activo "),
        ("Exited", "Salió"),
        ("Created", "Creado"),
        ("Restarting", "Reiniciando"),
        ("Paused", "Pausado"),
        ("Dead", "Muerto"),
        ("About an hour", "≈1 h"),
        ("About a minute", "≈1 min"),
        ("Less than a second", "<1 s"),
        (" seconds", " s"),
        (" second", " s"),
        (" minutes", " min"),
        (" minute", " min"),
        (" hours", " h"),
        (" hour", " h"),
        (" days", " d"),
        (" day", " d"),
        (" weeks", " sem"),
        (" week", " sem"),
        (" months", " meses"),
        (" month", " mes"),
        (" years", " años"),
        (" ago", " atrás"),
        ("(health: starting)", "(iniciando)"),
        ("(unhealthy)", "(con fallas)"),
        ("(healthy)", "(sano)"),
    ] {
        t = t.replace(a, b);
    }
    t
}

pub(super) fn state_dot(c: &CRow) -> Span<'static> {
    let (sym, col) = match (c.state, c.health) {
        (_, Some(Health::Unhealthy)) => ("●", ERRC),
        (St::Running, Some(Health::Starting)) => ("●", WARNC),
        (St::Running, _) => ("●", OKC),
        (St::Paused, _) | (St::Restarting, _) => ("●", WARNC),
        (St::Dead, _) => ("●", ERRC),
        _ => ("○", DIM),
    };
    Span::styled(sym, Style::default().fg(col))
}

