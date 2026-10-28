//! Entrada de texto de una línea con cursor, desplazamiento horizontal y atajos de edición.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

#[derive(Clone, Default, Debug)]
pub struct LineInput {
    pub buf: Vec<char>,
    pub cur: usize,
}

impl LineInput {
    pub fn new(s: &str) -> LineInput {
        let buf: Vec<char> = s.chars().collect();
        let cur = buf.len();
        LineInput { buf, cur }
    }

    pub fn text(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            self.buf.insert(self.cur, c);
            self.cur += 1;
        }
    }

    fn word_left(&self) -> usize {
        let mut i = self.cur;
        while i > 0 && self.buf[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.buf[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let mut i = self.cur;
        while i < self.buf.len() && self.buf[i].is_whitespace() {
            i += 1;
        }
        while i < self.buf.len() && !self.buf[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Procesa una tecla de edición. Devuelve true si la consumió.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if ctrl => match c {
                'a' => self.cur = 0,
                'e' => self.cur = self.buf.len(),
                'u' => {
                    self.buf.drain(..self.cur);
                    self.cur = 0;
                }
                'k' => self.buf.truncate(self.cur),
                'w' => {
                    let to = self.word_left();
                    self.buf.drain(to..self.cur);
                    self.cur = to;
                }
                _ => return false,
            },
            KeyCode::Char(c) if !alt => {
                self.buf.insert(self.cur, c);
                self.cur += 1;
            }
            KeyCode::Backspace => {
                if self.cur > 0 {
                    self.cur -= 1;
                    self.buf.remove(self.cur);
                }
            }
            KeyCode::Delete => {
                if self.cur < self.buf.len() {
                    self.buf.remove(self.cur);
                }
            }
            KeyCode::Left if ctrl || alt => self.cur = self.word_left(),
            KeyCode::Right if ctrl || alt => self.cur = self.word_right(),
            KeyCode::Left => self.cur = self.cur.saturating_sub(1),
            KeyCode::Right => self.cur = (self.cur + 1).min(self.buf.len()),
            KeyCode::Home => self.cur = 0,
            KeyCode::End => self.cur = self.buf.len(),
            _ => return false,
        }
        true
    }

    /// Spans listos para dibujar, con el cursor invertido, recortados a `width` columnas.
    pub fn spans(&self, width: usize, style: Style) -> Vec<Span<'static>> {
        let width = width.max(2);
        // ventana que siempre contiene el cursor
        let total = self.buf.len() + 1; // +1: posición final del cursor
        let start = (self.cur + 1).saturating_sub(width);
        let end = (start + width).min(total);
        let mut before = String::new();
        let mut at = String::from(" ");
        let mut after = String::new();
        for i in start..end {
            if i < self.cur {
                before.push(self.buf[i]);
            } else if i == self.cur {
                at = self.buf.get(i).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
            } else {
                after.push(self.buf[i]);
            }
        }
        vec![
            Span::styled(before, style),
            Span::styled(at, style.add_modifier(Modifier::REVERSED)),
            Span::styled(after, style),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers: m, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    #[test]
    fn editing() {
        let mut i = LineInput::new("hola");
        i.handle(k(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(i.text(), "hola!");
        i.handle(k(KeyCode::Left, KeyModifiers::NONE));
        i.handle(k(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(i.text(), "hol!");
        i.handle(k(KeyCode::Home, KeyModifiers::NONE));
        i.handle(k(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(i.text(), "ol!");
        i.handle(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "ol!");
        i.handle(k(KeyCode::End, KeyModifiers::NONE));
        i.handle(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "");
    }

    #[test]
    fn words_and_paste() {
        let mut i = LineInput::new("uno dos tres");
        i.handle(k(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "uno dos ");
        i.insert_str("a\nb\tc");
        assert_eq!(i.text(), "uno dos abc");
        let s = i.spans(5, Style::default());
        let joined: String = s.iter().map(|x| x.content.to_string()).collect();
        assert_eq!(joined.chars().count(), 5);
    }
}
