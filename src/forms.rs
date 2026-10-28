//! Formulario editable para despliegues nuevos y edición de contenedores existentes.

use crate::catalog::{Plan, PortSpec, RESTARTS, SvcPlan};
use crate::engine::EditSource;
use crate::input::LineInput;
use crate::model::Level;
use crate::util::{join_words, split_words};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Header(usize),
    Image(usize),
    Port(usize, usize),
    AddPort(usize),
    Env(usize, usize),
    AddEnv(usize),
    Vol(usize, usize),
    AddVol(usize),
    Restart(usize),
    Mem(usize),
    Cpus(usize),
    Cmd(usize),
}

impl Row {
    pub fn selectable(self) -> bool {
        !matches!(self, Row::Header(_))
    }
    pub fn is_add(self) -> bool {
        matches!(self, Row::AddPort(_) | Row::AddEnv(_) | Row::AddVol(_))
    }
}

pub enum FormKind {
    Deploy,
    Edit(Box<EditSource>),
}

pub struct Form {
    pub kind: FormKind,
    pub plan: Plan,
    pub rows: Vec<Row>,
    pub sel: usize,
    pub edit: Option<(LineInput, Row)>,
    pub msg: Option<(String, Level)>,
    /// Puertos del formulario que chocan con otros contenedores o con el sistema.
    pub port_issues: Vec<String>,
}

impl Form {
    pub fn new(kind: FormKind, plan: Plan) -> Form {
        let mut f = Form { kind, plan, rows: vec![], sel: 0, edit: None, msg: None, port_issues: vec![] };
        f.rebuild();
        f.sel = f.rows.iter().position(|r| r.selectable()).unwrap_or(0);
        f
    }

    pub fn is_edit(&self) -> bool {
        matches!(self.kind, FormKind::Edit(_))
    }

    pub fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let multi = self.plan.services.len() > 1;
        for (i, s) in self.plan.services.iter().enumerate() {
            if multi || !self.is_edit() {
                rows.push(Row::Header(i));
            }
            rows.push(Row::Image(i));
            for p in 0..s.ports.len() {
                rows.push(Row::Port(i, p));
            }
            rows.push(Row::AddPort(i));
            for e in 0..s.env.len() {
                rows.push(Row::Env(i, e));
            }
            rows.push(Row::AddEnv(i));
            for v in 0..s.volumes.len() {
                rows.push(Row::Vol(i, v));
            }
            rows.push(Row::AddVol(i));
            rows.push(Row::Restart(i));
            rows.push(Row::Mem(i));
            rows.push(Row::Cpus(i));
            rows.push(Row::Cmd(i));
        }
        let keep = self.rows.get(self.sel).copied();
        self.rows = rows;
        if let Some(k) = keep {
            if let Some(p) = self.rows.iter().position(|r| *r == k) {
                self.sel = p;
                return;
            }
        }
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
        if !self.rows.get(self.sel).map(|r| r.selectable()).unwrap_or(false) {
            self.sel = self.rows.iter().position(|r| r.selectable()).unwrap_or(0);
        }
    }

    pub fn current(&self) -> Option<Row> {
        self.rows.get(self.sel).copied()
    }

    pub fn move_sel(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        let mut i = self.sel as isize;
        loop {
            i += d;
            if i < 0 || i >= self.rows.len() as isize {
                return;
            }
            if self.rows[i as usize].selectable() {
                self.sel = i as usize;
                return;
            }
        }
    }

    pub fn first(&mut self) {
        self.sel = self.rows.iter().position(|r| r.selectable()).unwrap_or(0);
    }

    pub fn last(&mut self) {
        self.sel = self.rows.iter().rposition(|r| r.selectable()).unwrap_or(0);
    }

    /// (etiqueta, valor, ¿es valor vacío/ayuda?)
    pub fn row_text(&self, r: Row) -> (String, String, bool) {
        let svc = |i: usize| -> &SvcPlan { &self.plan.services[i] };
        match r {
            Row::Header(i) => {
                let s = svc(i);
                let who = if self.is_edit() { self.plan.name.clone() } else { self.plan.container_name(i) };
                (s.role.to_string(), who, false)
            }
            Row::Image(i) => ("Imagen".into(), svc(i).image.clone(), svc(i).image.is_empty()),
            Row::Port(i, p) => ("Puerto".into(), svc(i).ports[p].clone(), false),
            Row::AddPort(_) => ("".into(), "+ añadir puerto   (host:contenedor[/udp])".into(), true),
            Row::Env(i, e) => ("Variable".into(), svc(i).env[e].clone(), false),
            Row::AddEnv(_) => ("".into(), "+ añadir variable   (CLAVE=valor)".into(), true),
            Row::Vol(i, v) => ("Volumen".into(), svc(i).volumes[v].clone(), false),
            Row::AddVol(_) => ("".into(), "+ añadir volumen   (nombre:/ruta  o  /carpeta:/ruta[:ro])".into(), true),
            Row::Restart(i) => ("Reinicio".into(), svc(i).restart.clone(), false),
            Row::Mem(i) => {
                let m = &svc(i).mem;
                ("Memoria máx.".into(), if m.is_empty() { "sin límite".into() } else { m.clone() }, m.is_empty())
            }
            Row::Cpus(i) => {
                let c = &svc(i).cpus;
                ("CPU máx.".into(), if c.is_empty() { "sin límite   (núcleos: 0.5, 2…)".into() } else { format!("{c} núcleos") }, c.is_empty())
            }
            Row::Cmd(i) => {
                let c = join_words(&svc(i).cmd);
                ("Comando".into(), if c.is_empty() { "(el de la imagen)".into() } else { c.clone() }, c.is_empty())
            }
        }
    }

    fn edit_value(&self, r: Row) -> String {
        let s = |i: usize| &self.plan.services[i];
        match r {
            Row::Image(i) => s(i).image.clone(),
            Row::Port(i, p) => s(i).ports[p].clone(),
            Row::Env(i, e) => s(i).env[e].clone(),
            Row::Vol(i, v) => s(i).volumes[v].clone(),
            Row::Mem(i) => s(i).mem.clone(),
            Row::Cpus(i) => s(i).cpus.clone(),
            Row::Cmd(i) => join_words(&s(i).cmd),
            _ => String::new(),
        }
    }

    /// Enter sobre la fila actual: edita o alterna.
    pub fn activate(&mut self) {
        let Some(r) = self.current() else { return };
        match r {
            Row::Header(_) => {}
            Row::Restart(i) => {
                let cur = RESTARTS.iter().position(|x| *x == self.plan.services[i].restart).unwrap_or(0);
                self.plan.services[i].restart = RESTARTS[(cur + 1) % RESTARTS.len()].to_string();
            }
            r if r.is_add() => self.edit = Some((LineInput::new(""), r)),
            r => self.edit = Some((LineInput::new(&self.edit_value(r)), r)),
        }
    }

    /// Confirma la edición en curso.
    pub fn commit_edit(&mut self) {
        let Some((input, r)) = self.edit.take() else { return };
        let text = input.text().trim().to_string();
        match r {
            Row::Image(i) => self.plan.services[i].image = text,
            Row::Mem(i) => self.plan.services[i].mem = text,
            Row::Cpus(i) => self.plan.services[i].cpus = text,
            Row::Cmd(i) => self.plan.services[i].cmd = split_words(&text),
            Row::AddPort(i) => {
                if !text.is_empty() {
                    self.plan.services[i].ports.push(text);
                }
            }
            Row::AddEnv(i) => {
                if !text.is_empty() {
                    self.plan.services[i].env.push(text);
                }
            }
            Row::AddVol(i) => {
                if !text.is_empty() {
                    self.plan.services[i].volumes.push(text);
                }
            }
            Row::Port(i, p) => {
                if text.is_empty() {
                    self.plan.services[i].ports.remove(p);
                } else {
                    self.plan.services[i].ports[p] = text;
                }
            }
            Row::Env(i, e) => {
                if text.is_empty() {
                    self.plan.services[i].env.remove(e);
                } else {
                    self.plan.services[i].env[e] = text;
                }
            }
            Row::Vol(i, v) => {
                if text.is_empty() {
                    self.plan.services[i].volumes.remove(v);
                } else {
                    self.plan.services[i].volumes[v] = text;
                }
            }
            _ => {}
        }
        self.msg = None;
        self.rebuild();
        // tras añadir, quedarse en la fila "añadir" para encadenar entradas
        if r.is_add() {
            if let Some(p) = self.rows.iter().position(|x| *x == r) {
                self.sel = p;
            }
        }
    }

    pub fn delete_current(&mut self) {
        let Some(r) = self.current() else { return };
        match r {
            Row::Port(i, p) => {
                self.plan.services[i].ports.remove(p);
            }
            Row::Env(i, e) => {
                self.plan.services[i].env.remove(e);
            }
            Row::Vol(i, v) => {
                self.plan.services[i].volumes.remove(v);
            }
            _ => return,
        }
        self.rebuild();
    }

    pub fn port_has_issue(&self, text: &str) -> bool {
        PortSpec::parse(text).ok().and_then(|p| p.host).map(|h| self.port_issues.iter().any(|x| x == &format!("{h}"))).unwrap_or(false)
    }

    /// Resumen de errores y avisos para mostrar bajo el formulario.
    pub fn problems(&self) -> Vec<(String, Level)> {
        let mut v: Vec<(String, Level)> = self.plan.errors().into_iter().map(|e| (e, Level::Err)).collect();
        for p in &self.port_issues {
            v.push((format!("el puerto {p} ya está en uso"), Level::Err));
        }
        for w in self.plan.warnings() {
            v.push((w, Level::Warn));
        }
        v
    }

    pub fn blocking(&self) -> bool {
        self.problems().iter().any(|(_, l)| *l == Level::Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn plan() -> Plan {
        let (cat, _) = Catalog::load();
        Plan::build(cat.get("postgres").unwrap(), "pg", None, "unless-stopped")
    }

    #[test]
    fn navigate_and_edit() {
        let mut f = Form::new(FormKind::Deploy, plan());
        // Header se salta
        assert!(matches!(f.current(), Some(Row::Image(0))));
        f.activate();
        assert!(f.edit.is_some());
        f.edit.as_mut().unwrap().0 = LineInput::new("postgres:16-alpine");
        f.commit_edit();
        assert_eq!(f.plan.services[0].image, "postgres:16-alpine");

        // añadir variable
        while !matches!(f.current(), Some(Row::AddEnv(0))) {
            f.move_sel(1);
        }
        f.activate();
        f.edit.as_mut().unwrap().0 = LineInput::new("FOO=bar");
        f.commit_edit();
        assert!(f.plan.services[0].env.contains(&"FOO=bar".to_string()));
        assert!(matches!(f.current(), Some(Row::AddEnv(0))));

        // borrar la variable recién añadida
        f.move_sel(-1);
        assert!(matches!(f.current(), Some(Row::Env(0, _))));
        f.delete_current();
        assert!(!f.plan.services[0].env.contains(&"FOO=bar".to_string()));
    }

    #[test]
    fn restart_cycles_and_empty_deletes() {
        let mut f = Form::new(FormKind::Deploy, plan());
        while !matches!(f.current(), Some(Row::Restart(0))) {
            f.move_sel(1);
        }
        f.activate();
        assert_eq!(f.plan.services[0].restart, "always");
        // editar un puerto y dejarlo vacío lo borra
        f.first();
        f.move_sel(1);
        assert!(matches!(f.current(), Some(Row::Port(0, 0))));
        f.activate();
        f.edit.as_mut().unwrap().0 = LineInput::new("");
        f.commit_edit();
        assert!(f.plan.services[0].ports.is_empty());
    }

    #[test]
    fn cpu_limit_row_validates() {
        let mut f = Form::new(FormKind::Deploy, plan());
        while !matches!(f.current(), Some(Row::Cpus(0))) {
            f.move_sel(1);
        }
        f.activate();
        f.edit.as_mut().unwrap().0 = LineInput::new("1.5");
        f.commit_edit();
        assert_eq!(f.plan.services[0].cpus, "1.5");
        assert!(!f.blocking());
        f.activate();
        f.edit.as_mut().unwrap().0 = LineInput::new("mucho");
        f.commit_edit();
        assert!(f.blocking());
    }

    #[test]
    fn blocking_on_errors() {
        let mut f = Form::new(FormKind::Deploy, plan());
        assert!(!f.blocking());
        f.plan.services[0].image.clear();
        assert!(f.blocking());
        f.plan.services[0].image = "x".into();
        f.port_issues.push("5432".into());
        assert!(f.blocking());
    }
}
