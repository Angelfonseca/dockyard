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

const ACC: Color = Color::Cyan;
const OKC: Color = Color::Green;
const WARNC: Color = Color::Yellow;
const ERRC: Color = Color::Red;
const DIM: Color = Color::DarkGray;

fn lvl_color(l: Level) -> Color {
    match l {
        Level::Info => ACC,
        Level::Ok => OKC,
        Level::Warn => WARNC,
        Level::Err => ERRC,
    }
}

fn pct_color(p: f64) -> Color {
    if p < 60.0 {
        OKC
    } else if p < 85.0 {
        WARNC
    } else {
        ERRC
    }
}

fn dim() -> Style {
    Style::default().fg(DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn sel_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

fn bar(ratio: f64, w: usize, color: Color) -> Vec<Span<'static>> {
    let r = ratio.clamp(0.0, 1.0);
    let filled = ((r * w as f64).round() as usize).min(w);
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(w - filled), dim()),
    ]
}

fn block<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title(title)
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn spinner(app: &App) -> &'static str {
    const F: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    F[(app.started.elapsed().as_millis() / 90) as usize % F.len()]
}

fn es_status(s: &str) -> String {
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

fn state_dot(c: &CRow) -> Span<'static> {
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

// ───────────────────────────── Panel ─────────────────────────────

fn spark<'a>(data: &'a std::collections::VecDeque<u64>, max: u64, color: Color, w: usize) -> Sparkline<'a> {
    let skip = data.len().saturating_sub(w);
    Sparkline::default().data(data.iter().skip(skip).copied()).max(max.max(1)).style(Style::default().fg(color))
}

fn draw_panel(f: &mut Frame, area: Rect, app: &mut App) {
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

// ───────────────────────────── Contenedores ─────────────────────────────

fn draw_containers(f: &mut Frame, area: Rect, app: &mut App) {
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

fn kv(k: &str, v: String) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k:<9}"), dim()), Span::raw(v)])
}

fn draw_container_detail(f: &mut Frame, area: Rect, app: &App) {
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

// ───────────────────────────── Imágenes / Volúmenes / Catálogo ─────────────────────────────

fn draw_images(f: &mut Frame, area: Rect, app: &mut App) {
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

fn draw_volumes(f: &mut Frame, area: Rect, app: &mut App) {
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

fn draw_catalog(f: &mut Frame, area: Rect, app: &mut App) {
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

// ───────────────────────────── Espacio ─────────────────────────────

fn draw_space(f: &mut Frame, area: Rect, app: &mut App) {
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

// ───────────────────────────── Logs y texto ─────────────────────────────

fn log_style(l: &str) -> (String, Style) {
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

fn draw_logs(f: &mut Frame, area: Rect, lv: &LogView) {
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

fn draw_text(f: &mut Frame, area: Rect, tv: &TextView) {
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

// ───────────────────────────── Formulario ─────────────────────────────

fn draw_form(f: &mut Frame, area: Rect, fm: &Form, _app: &App) {
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

// ───────────────────────────── Ventanas emergentes ─────────────────────────────

fn draw_prompt(f: &mut Frame, area: Rect, p: &Prompt) {
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
    spans.extend(p.input.spans(a.width.saturating_sub(3) as usize, Style::default()));
    f.render_widget(Paragraph::new(Line::from(spans)), a);
    let _ = b;
    f.render_widget(Paragraph::new(Span::styled(truncate(&p.hint, c.width as usize), dim())), c);
}

fn draw_confirm(f: &mut Frame, area: Rect, c: &Confirm) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Confirm, ConfirmAction, Mode, Prompt, PromptAction, Tab, TextView};
    use crate::catalog::{Catalog, Plan};
    use crate::collector::Shared;
    use crate::config::Config;
    use crate::engine::Engine;
    use crate::forms::{Form, FormKind};
    use crate::input::LineInput;
    use crate::model::{CRow, CStat, DiskInfo, Health, Host, IRow, Snapshot, St, VRow};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    fn crow(i: usize, state: St) -> CRow {
        CRow {
            id: format!("{i:064x}"),
            name: format!("servicio-{i}"),
            image: "postgres:17-alpine".into(),
            image_id: "sha256:abc".into(),
            state,
            status: "Up 3 hours".into(),
            health: if i % 2 == 0 { Some(Health::Healthy) } else { None },
            ports: "0.0.0.0:5432->5432/tcp".into(),
            published: vec![(5432 + i as u16, "tcp".into())],
            stack: if i % 3 == 0 { Some("nc".into()) } else { None },
            created: 1_700_000_000,
            mounts: vec!["pg-data".into()],
            networks: vec!["bridge".into()],
        }
    }

    fn app() -> App {
        let engine = Arc::new(Engine::offline());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (cfg, _) = Config::load();
        let (cat, _) = Catalog::load();
        let shared = Shared::new(cfg.refresh_ms());
        let mut a = App::new(engine, tx, shared, cfg, cat);
        let mut snap = Snapshot::default();
        for i in 0..12 {
            let st = if i % 4 == 3 { St::Exited } else { St::Running };
            let c = crow(i, st);
            if st == St::Running {
                snap.stats.insert(
                    c.id.clone(),
                    CStat { cpu: 3.0 * i as f64, mem: 80 << 20, mem_limit: 1 << 30, rx: 1 << 20, tx: 1 << 19, rx_rate: 1e5, tx_rate: 2e4 },
                );
            }
            snap.containers.push(c);
        }
        snap.host = Host {
            hostname: "fserver".into(),
            os: "Arch Linux".into(),
            cpu: 37.0,
            cores: vec![10.0, 80.0, 5.0, 33.0, 12.0, 90.0, 1.0, 50.0],
            cpu_model: "AMD Ryzen 7 2700U".into(),
            temp_c: Some(54.0),
            mem_used: 3 << 30,
            mem_total: 8 << 30,
            swap_used: 0,
            swap_total: 4 << 30,
            load: (0.5, 0.7, 0.9),
            uptime: 90_000,
            disks: vec![DiskInfo { mount: "/".into(), used: 40 << 30, total: 230 << 30 }],
            net_rx_rate: 1e6,
            net_tx_rate: 3e5,
        };
        snap.docker_version = "27.0".into();
        snap.images = Some(vec![IRow { id: "sha256:abc".into(), tags: vec!["postgres:17-alpine".into()], size: 250 << 20, created: 1_700_000_000, used_by: vec!["servicio-0".into()] }]);
        snap.volumes = Some(vec![VRow { name: "pg-data".into(), driver: "local".into(), mountpoint: "/var/lib/docker/volumes/pg-data/_data".into(), used_by: vec!["servicio-0".into()] }]);
        a.connected = true;
        a.apply_snapshot(snap);
        a
    }

    fn render(a: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, a)).unwrap();
        let buf = t.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[tokio::test]
    async fn all_tabs_all_sizes() {
        let mut a = app();
        for tab in Tab::ALL {
            a.tab = tab;
            for (w, h) in [(160, 50), (120, 40), (80, 24), (50, 14), (20, 6), (1, 1)] {
                let s = render(&mut a, w, h);
                assert_eq!(s.lines().count(), h as usize);
            }
        }
        a.tab = Tab::Containers;
        let s = render(&mut a, 120, 40);
        assert!(s.contains("servicio-0"), "{s}");
        a.tab = Tab::Panel;
        let s = render(&mut a, 120, 40);
        assert!(s.contains("fserver") || s.contains("Ryzen"), "{s}");
        a.tab = Tab::Catalog;
        let s = render(&mut a, 120, 40);
        assert!(s.to_lowercase().contains("postgres"), "{s}");
    }

    #[tokio::test]
    async fn overlays_render() {
        let mut a = app();
        let (cat, _) = Catalog::load();
        let plan = Plan::build(cat.get("postgres").unwrap(), "pg", None, "unless-stopped");
        let mut form = Form::new(FormKind::Deploy, plan);
        form.port_issues.push("5432".into());
        a.mode = Mode::Form(Box::new(form));
        for (w, h) in [(120, 40), (80, 24), (40, 10)] {
            let s = render(&mut a, w, h);
            if w >= 80 {
                assert!(s.contains("POSTGRES_PASSWORD"), "{s}");
            }
        }
        a.mode = Mode::Text(Box::new(TextView { title: "inspect".into(), lines: (0..200).map(|i| format!("línea {i} áéí")).collect(), scroll: 150, hscroll: 3 }));
        render(&mut a, 100, 30);
        render(&mut a, 30, 8);
        a.mode = Mode::Prompt(Box::new(Prompt { title: "Descargar".into(), hint: "imagen".into(), input: LineInput::new("redis:7"), action: PromptAction::Pull }));
        let s = render(&mut a, 100, 30);
        assert!(s.contains("redis:7"), "{s}");
        a.mode = Mode::Confirm(Box::new(Confirm { title: "Eliminar".into(), lines: vec!["¿Seguro?".into()], action: ConfirmAction::PruneImages, danger: true, alt: None }));
        let s = render(&mut a, 100, 30);
        assert!(s.contains("Seguro"), "{s}");
        render(&mut a, 20, 5);
        a.mode = Mode::Confirm(Box::new(Confirm {
            title: "Restaurar".into(),
            lines: vec!["volumen".into()],
            action: ConfirmAction::PruneNetworks,
            danger: true,
            alt: Some(('l', "Vaciar y restaurar".into())),
        }));
        let s = render(&mut a, 100, 30);
        assert!(s.contains("Vaciar y restaurar"), "{s}");
    }

    #[tokio::test]
    async fn space_tab_and_task_view() {
        use crate::model::{DiskUsage, Usage};
        let mut a = app();
        a.tab = Tab::Space;
        let s = render(&mut a, 120, 40);
        assert!(s.contains("Calculando"), "{s}");
        let us = |n: u64| Usage { count: n, active: 1, size: n << 30, reclaimable: n << 29, known: true };
        let mut u = DiskUsage { images: us(4), containers: us(2), volumes: us(3), build: Usage::default(), ..Default::default() };
        u.vol_sizes.insert("pg-data".into(), 5 << 30);
        a.usage = Some(u);
        for (w, h) in [(160, 50), (120, 40), (80, 24), (50, 14)] {
            render(&mut a, w, h);
        }
        let s = render(&mut a, 140, 40);
        assert!(s.contains("Espacio de Docker") && s.contains("pg-data") && s.contains("n/d"), "{s}");
        a.tab = Tab::Volumes;
        let s = render(&mut a, 140, 40);
        assert!(s.contains("5.00G"), "{s}");

        let mut lv = a.start_task_view("srv $ echo hola".into(), |mut out| async move {
            out("hola".into());
            Ok("✔ listo".into())
        });
        lv.lines.push_back("hola".into());
        lv.ended = Some("✔ terminó (código 0)".into());
        lv.ended_ok = true;
        a.mode = Mode::Logs(Box::new(lv));
        let s = render(&mut a, 100, 30);
        assert!(s.contains("echo hola") && s.contains("terminó (código 0)"), "{s}");
        render(&mut a, 40, 10);
    }
}
