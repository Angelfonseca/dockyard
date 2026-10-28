//! Bucle principal de la TUI: entrada de teclado en un hilo, mensajes de tareas y dibujado.

use crate::app::{App, Pending};
use crate::catalog::Catalog;
use crate::collector::{self, Shared};
use crate::config::Config;
use crate::engine::Engine;
use crate::model::*;
use crate::ui;
use crate::{ext, shell};
use anyhow::Result;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use std::io::stdout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

struct InputThread {
    rx: mpsc::UnboundedReceiver<Event>,
    pause: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

fn spawn_input() -> InputThread {
    let (tx, rx) = mpsc::unbounded_channel();
    let pause = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let (p, pd) = (pause.clone(), paused.clone());
    std::thread::spawn(move || {
        loop {
            if p.load(Ordering::SeqCst) {
                pd.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            pd.store(false, Ordering::SeqCst);
            match event::poll(Duration::from_millis(80)) {
                Ok(true) => {
                    if let Ok(ev) = event::read() {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                }
                Ok(false) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    InputThread { rx, pause, paused }
}

impl InputThread {
    /// Detiene la lectura de teclado para ceder la terminal a un proceso hijo.
    fn suspend(&self) {
        self.pause.store(true, Ordering::SeqCst);
        for _ in 0..100 {
            if self.paused.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn resume(&self) {
        self.pause.store(false, Ordering::SeqCst);
    }
}

fn enter_terminal() -> ratatui::DefaultTerminal {
    let t = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    t
}

fn leave_terminal() {
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
}

pub async fn run() -> Result<()> {
    let (cfg, cfg_warn) = Config::load();
    let mut engine = Engine::connect().await?;
    engine.log_rotation = cfg.log_rotation();
    let engine = Arc::new(engine);
    let (catalog, cat_warn) = Catalog::load();
    let shared = Shared::new(cfg.refresh_ms());
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    tokio::spawn(collector::run(engine.clone(), shared.clone(), tx.clone()));
    tokio::spawn(collector::run_usage(engine.clone(), shared.clone(), tx.clone()));
    engine.watch_events(shared.notify.clone());

    let mut app = App::new(engine.clone(), tx.clone(), shared.clone(), cfg, catalog);
    if let Some(w) = cfg_warn.or(cat_warn) {
        app.toast(format!("Error en configuración: {w}"), Level::Warn);
    }

    let mut input = spawn_input();
    let mut terminal = enter_terminal();
    // Si algo entra en pánico, dejar de capturar el pegado además de restaurar la terminal.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        let _ = execute!(stdout(), DisableBracketedPaste);
        prev(i);
    }));

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let result: Result<()> = async {
        loop {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            tokio::select! {
                Some(ev) = input.rx.recv() => {
                    handle_event(&mut app, ev);
                    while let Ok(ev) = input.rx.try_recv() {
                        handle_event(&mut app, ev);
                    }
                }
                Some(m) = rx.recv() => {
                    app.on_msg(m);
                    while let Ok(m) = rx.try_recv() {
                        app.on_msg(m);
                    }
                }
                _ = tick.tick() => {}
            }
            if let Some(p) = app.pending.take() {
                match p {
                    Pending::Shell { id, name } => {
                        input.suspend();
                        leave_terminal();
                        println!("\x1b[36m⚓ Shell de {name}\x1b[0m — escribe «exit» o Ctrl+D para volver a dockyard\n");
                        let r = shell::interactive(&engine, &id, shell::default_shell()).await;
                        terminal = enter_terminal();
                        input.resume();
                        match r {
                            Ok(Some(126 | 127)) => app.toast(crate::engine::missing_binary_hint("executable file not found").unwrap_or_default(), Level::Warn),
                            Ok(_) => app.toast(format!("Volviste de la shell de {name}"), Level::Info),
                            Err(e) => app.toast(format!("Shell de {name}: {e:#}"), Level::Err),
                        }
                    }
                    Pending::HostShell => {
                        input.suspend();
                        leave_terminal();
                        let where_ = engine.ssh_target().map(|(t, _)| t).unwrap_or_else(|| "esta máquina".into());
                        println!("\x1b[36m⚓ Shell de {where_}\x1b[0m — escribe «exit» para volver a dockyard\n");
                        let st = ext::host_shell(&engine).status();
                        terminal = enter_terminal();
                        input.resume();
                        match st {
                            Ok(_) => app.toast(format!("Volviste de la shell de {where_}"), Level::Info),
                            Err(e) => app.toast(format!("No se pudo abrir la shell: {e}"), Level::Err),
                        }
                    }
                    Pending::EditFile { id, name, path, volume } => {
                        terminal.draw(|f| ui::draw(f, &mut app))?;
                        let r = edit_file(&engine, &id, &path, volume, &input).await;
                        terminal = enter_terminal();
                        input.resume();
                        match r {
                            Ok(true) if volume => app.toast(format!("{path} guardado en el volumen {name}"), Level::Ok),
                            Ok(true) => app.toast(format!("{path} guardado; reinicia {name} (r) para aplicar"), Level::Ok),
                            Ok(false) => app.toast(format!("Sin cambios en {path}"), Level::Info),
                            Err(e) => app.toast(format!("{name}: {e:#}"), Level::Err),
                        }
                    }
                }
            }
            if app.should_quit {
                break;
            }
        }
        Ok(())
    }
    .await;
    leave_terminal();
    result
}

/// Baja el archivo (de un contenedor, o de un volumen con un auxiliar), lo abre en el editor con la
/// TUI suspendida y lo sube si cambió. Deja la terminal fuera de la TUI: el llamador la restaura.
async fn edit_file(engine: &Engine, id: &str, path: &str, volume: bool, input: &InputThread) -> Result<bool> {
    let (container, inner) = if volume {
        let mut quiet = |_s: String| {};
        let h = engine.create_helper(vec![format!("{id}:/data")], None, &mut quiet).await?;
        (h, format!("/data/{}", path.trim_start_matches('/')))
    } else {
        (id.to_string(), path.to_string())
    };
    let r: Result<bool> = async {
        let (content, header, _new) = engine.read_file(&container, &inner).await?;
        let fname = std::path::Path::new(&inner).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "archivo".into());
        input.suspend();
        leave_terminal();
        let edited = tokio::task::spawn_blocking(move || ext::edit_in_editor(&content, &fname)).await??;
        match edited {
            Some(new) => {
                engine.write_file(&container, &inner, &new, &header).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
    .await;
    // si falló antes de abrir el editor, la entrada sigue activa: suspender para que resume() cuadre
    input.suspend();
    leave_terminal();
    if volume {
        engine.remove_helper(&container).await;
    }
    r
}

fn handle_event(app: &mut App, ev: Event) {
    match ev {
        Event::Key(k) => app.on_key(k),
        Event::Paste(s) => app.on_paste(&s),
        _ => {}
    }
}
