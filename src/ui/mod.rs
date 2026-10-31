//! Dibujado de la TUI con ratatui.

use crate::app::*;
use crate::forms::{Form, Row as FRow};
use crate::model::*;
use crate::util::*;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline,
    Table, Tabs, Wrap,
};

mod style;
mod panel;
mod containers;
mod lists;
mod space;
mod logs;
mod form;
mod popups;

use style::*;
use panel::*;
use containers::*;
use lists::*;
use space::*;
use logs::*;
use form::*;
use popups::*;

// ───────────────────────────── Principal ─────────────────────────────

pub fn draw(f: &mut Frame, app: &mut App) {
    app.frame = app.frame.wrapping_add(1);
    app.sync_selections();
    let area = f.area();
    if area.width < 40 || area.height < 10 {
        f.render_widget(Paragraph::new("Terminal muy pequeña (mín. 40×10)"), area);
        return;
    }
    let [head, body, status, keys] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)]).areas(area);
    draw_header(f, head, app);

    let mode = std::mem::replace(&mut app.mode, Mode::Normal);
    match &mode {
        Mode::Logs(lv) => draw_logs(f, body, lv),
        Mode::Text(tv) => draw_text(f, body, tv),
        Mode::Form(fm) => draw_form(f, body, fm, app),
        _ => match app.tab {
            Tab::Panel => draw_panel(f, body, app),
            Tab::Containers => draw_containers(f, body, app),
            Tab::Images => draw_images(f, body, app),
            Tab::Volumes => draw_volumes(f, body, app),
            Tab::Catalog => draw_catalog(f, body, app),
            Tab::Space => draw_space(f, body, app),
        },
    }
    draw_status(f, status, app);
    draw_keys(f, keys, app, &mode);
    match &mode {
        Mode::Prompt(p) => draw_prompt(f, area, p),
        Mode::Confirm(c) => draw_confirm(f, area, c),
        _ => {}
    }
    app.mode = mode;
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let brand = " ⚓ dockyard ";
    let up = app.snap.containers.iter().filter(|c| c.state == St::Running).count();
    let total = app.snap.containers.len();
    let right = if app.connected {
        format!(" {} · {up}/{total} activos ", if app.snap.host.hostname.is_empty() { "host" } else { &app.snap.host.hostname })
    } else {
        " sin conexión ".to_string()
    };
    let [a, b, c] = Layout::horizontal([
        Constraint::Length(brand.chars().count() as u16),
        Constraint::Min(10),
        Constraint::Length(right.chars().count() as u16),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(brand).style(Style::default().fg(Color::Black).bg(ACC).add_modifier(Modifier::BOLD)), a);
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            let n = match t {
                Tab::Containers => format!(" {}", app.snap.containers.len()),
                Tab::Images => {
                    if app.images.is_empty() { String::new() } else { format!(" {}", app.images.len()) }
                }
                Tab::Volumes => {
                    if app.volumes.is_empty() { String::new() } else { format!(" {}", app.volumes.len()) }
                }
                _ => String::new(),
            };
            Line::from(format!("{} {}{}", t.index() + 1, t.title(), n))
        })
        .collect();
    f.render_widget(
        Tabs::new(titles)
            .select(app.tab.index())
            .style(dim())
            .highlight_style(Style::default().fg(ACC).add_modifier(Modifier::BOLD | Modifier::UNDERLINED))
            .divider(" "),
        b,
    );
    let col = if app.connected { DIM } else { ERRC };
    f.render_widget(Paragraph::new(right).style(Style::default().fg(col)).alignment(Alignment::Right), c);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let w = area.width as usize;
    let line = if let Some(p) = &app.progress {
        Line::from(vec![Span::styled(format!(" {} ", spinner(app)), Style::default().fg(ACC)), Span::styled(truncate(p, w.saturating_sub(4)), Style::default().fg(ACC))])
    } else if let Some((m, l, _)) = app.toast.as_ref().filter(|(_, _, t)| t.elapsed() < std::time::Duration::from_secs(9)) {
        let icon = match l {
            Level::Ok => "✔",
            Level::Err => "✘",
            Level::Warn => "!",
            Level::Info => "i",
        };
        Line::from(Span::styled(format!(" {icon} {}", truncate(m, w.saturating_sub(4))), Style::default().fg(lvl_color(*l))))
    } else if !app.connected {
        let e = app.conn_err.clone().unwrap_or_else(|| "conectando…".into());
        Line::from(Span::styled(format!(" ✘ Docker no disponible: {}", truncate(&e, w.saturating_sub(26))), Style::default().fg(ERRC)))
    } else {
        Line::from(Span::styled(
            format!(" Docker {} · actualiza cada {}s · ? ayuda", app.snap.docker_version, app.cfg.refresh_ms() / 1000),
            dim(),
        ))
    };
    f.render_widget(Paragraph::new(line), area);
}

fn hint_line(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in pairs {
        spans.push(Span::styled(k.to_string(), Style::default().fg(ACC).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {d}  "), dim()));
    }
    Line::from(spans)
}

fn draw_keys(f: &mut Frame, area: Rect, app: &App, mode: &Mode) {
    let pairs: Vec<(&str, &str)> = match mode {
        Mode::Logs(lv) if lv.filter_input.is_some() => vec![("Enter", "aplicar"), ("Esc", "cancelar")],
        Mode::Logs(lv) if lv.kind == LogKind::Command => {
            if lv.ended.is_some() {
                vec![("f", "seguir"), ("/", "filtrar"), ("↑↓ PgUp", "mover"), ("w", "ajustar"), ("Esc", "cerrar")]
            } else {
                vec![("f", "seguir"), ("/", "filtrar"), ("↑↓ PgUp", "mover"), ("w", "ajustar"), ("Esc", "detener y cerrar")]
            }
        }
        Mode::Logs(_) => vec![("f", "seguir"), ("/", "filtrar"), ("↑↓ PgUp", "mover"), ("w", "ajustar"), ("t", "hora"), ("c", "limpiar"), ("Esc", "cerrar")],
        Mode::Text(_) => vec![("↑↓ PgUp PgDn", "mover"), ("←→", "lateral"), ("Esc", "cerrar")],
        Mode::Prompt(_) => vec![("Enter", "aceptar"), ("Esc", "cancelar")],
        Mode::Confirm(c) => match &c.alt {
            Some((k, d)) => vec![("s/Enter", "sí"), (if *k == 'l' { "l" } else { "alt" }, d.as_str()), ("n/Esc", "no")],
            None => vec![("s/Enter", "sí"), ("n/Esc", "no")],
        },
        Mode::Form(fm) if fm.edit.is_some() => vec![("Enter", "guardar"), ("Esc", "cancelar"), ("Ctrl+U", "borrar todo")],
        Mode::Form(_) => vec![("↑↓", "mover"), ("Enter", "editar"), ("a", "variable"), ("d", "borrar"), ("F5", "aplicar"), ("Esc", "cancelar")],
        Mode::Normal => match app.tab {
            Tab::Panel => vec![("1-6", "pestañas"), ("Enter", "contenedores"), ("X", "shell del host"), ("!", "comando"), ("?", "ayuda"), ("q", "salir")],
            Tab::Containers => vec![("Enter", "logs"), ("s", "ini/det"), ("r", "reiniciar"), ("e", "editar"), ("x", "shell"), ("c", "cmd"), ("F", "archivo"), ("u", "actualizar"), ("b", "respaldo"), ("R", "restaurar"), ("C", "claves"), ("d", "borrar"), ("?", "más")],
            Tab::Images => vec![("p", "descargar"), ("Enter", "crear contenedor"), ("d", "borrar"), ("D", "limpiar"), ("/", "filtro"), ("?", "ayuda")],
            Tab::Volumes => vec![("Enter", "explorar"), ("f", "editar archivo"), ("b", "respaldar"), ("R", "restaurar"), ("c", "clonar"), ("d", "borrar"), ("D", "huérfanos"), ("?", "ayuda")],
            Tab::Catalog => vec![("Enter", "instalar"), ("r", "desde Git"), ("o", "Compose"), ("/", "buscar"), ("?", "ayuda")],
            Tab::Space => vec![("c", "contenedores"), ("i", "imágenes"), ("b", "caché build"), ("n", "redes"), ("v", "volúmenes"), ("r", "recalcular"), ("?", "ayuda")],
        },
    };
    f.render_widget(Paragraph::new(hint_line(&pairs)), area);
}


#[cfg(test)]
mod tests;
