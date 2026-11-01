//! Mensajes que llegan de los trabajos y del recolector (`Msg`).

use super::*;

impl App {
    // ───────────────────────────── Mensajes de tareas ─────────────────────────────

    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Snapshot(s) => self.apply_snapshot(*s),
            Msg::Disconnected(e) => {
                self.connected = false;
                self.conn_err = Some(e);
            }
            Msg::Toast(s, l) => {
                self.progress = None;
                self.toast(s, l);
            }
            Msg::Progress(s) => self.progress = Some(s),
            Msg::Text { title, body } => {
                self.progress = None;
                self.mode = Mode::Text(Box::new(TextView {
                    title,
                    lines: body.lines().map(|l| l.to_string()).collect(),
                    scroll: 0,
                    hscroll: 0,
                }));
            }
            Msg::Log { session, lines } => {
                if let Mode::Logs(lv) = &mut self.mode {
                    if lv.session == session {
                        lv.total += lines.len() as u64;
                        for l in lines {
                            if lv.lines.len() >= LOG_CAP {
                                lv.lines.pop_front();
                                if !lv.follow {
                                    lv.scroll = lv.scroll.saturating_sub(1);
                                }
                            }
                            lv.lines.push_back(l);
                        }
                    }
                }
            }
            Msg::LogEnd { session, msg, ok } => {
                if let Mode::Logs(lv) = &mut self.mode {
                    if lv.session == session {
                        lv.ended = Some(msg);
                        lv.ended_ok = ok;
                    }
                }
                self.refresh_now();
            }
            Msg::EditLoaded(src, plan) => {
                self.progress = None;
                let mut f = Form::new(FormKind::Edit(src), *plan);
                self.check_ports(&mut f);
                self.mode = Mode::Form(Box::new(f));
                self.set_extra();
            }
            Msg::Usage(u) => self.usage = Some(*u),
            Msg::DeployForm(plan) => {
                self.progress = None;
                let mut f = Form::new(FormKind::Deploy, *plan);
                self.check_ports(&mut f);
                self.mode = Mode::Form(Box::new(f));
                self.set_extra();
            }
        }
    }

}
