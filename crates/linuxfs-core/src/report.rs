//! Sběr chyb a upozornění při výpisu a rozbalování.
//!
//! Obecný princip obou pluginů: **chyba jedné položky (nebo jen části svazku) nezastaví zbytek práce**. Co se nepodařilo,
//! se zapíše sem; na konci operace plugin ukáže shrnutí v okně a úplný seznam uloží do textového souboru v `%TEMP%`
//! (okno je krátké, soubor obsahuje všechno). Tento modul nezná žádné okno ani API hostitele, jen skládá texty a píše soubor.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::util;

/// Kolik řádků se ukáže přímo v okně (zbytek je v souboru).
pub const WINDOW_LINES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Kde: cesta položky nebo adresáře (v podobě, jak ji vidí panel), případně označení oddílu.
    pub place: String,
    pub message: String,
}

#[derive(Debug, Default, Clone)]
pub struct ProblemLog {
    items: Vec<Problem>,
}

impl ProblemLog {
    pub fn add(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.items.push(Problem {
            place: place.into(),
            message: message.into(),
        });
    }

    /// Upozornění z výpisu jednoho adresáře (`Listing::warnings`) s cestou adresáře.
    pub fn add_warnings(&mut self, dir: &str, warnings: impl IntoIterator<Item = String>) {
        for w in warnings {
            self.add(dir, w);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn items(&self) -> &[Problem] {
        &self.items
    }

    pub fn extend(&mut self, other: &ProblemLog) {
        self.items.extend(other.items.iter().cloned());
    }

    fn line(p: &Problem) -> String {
        let place = if p.place.is_empty() { "\\" } else { p.place.as_str() };
        format!("{place}: {}", p.message)
    }

    /// Text do okna: nadpis, prvních pár položek, počet zbývajících a odkaz na soubor s úplným seznamem.
    pub fn window_text(&self, headline: &str, file: Option<&Path>) -> String {
        let mut t = format!(
            "{} problem{} {headline}\n",
            self.items.len(),
            if self.items.len() == 1 { "" } else { "s" }
        );
        for p in self.items.iter().take(WINDOW_LINES) {
            t.push('\n');
            t.push_str(&Self::line(p));
        }
        if self.items.len() > WINDOW_LINES {
            t.push_str(&format!("\n... and {} more", self.items.len() - WINDOW_LINES));
        }
        match file {
            Some(f) => t.push_str(&format!("\n\nFull list: {}", f.display())),
            None => t.push_str("\n\n(The full list could not be saved to a file.)"),
        }
        t.push_str("\n(Ctrl+C copies this text.)");
        t
    }

    /// Úplný obsah souboru s hlášením.
    pub fn file_text(&self, title: &str, archive: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let mut t = format!(
            "linuxfs-plugins: {title}\r\nSource: {archive}\r\nTime: {} UTC\r\nProblems: {}\r\n\r\n",
            util::format_utc(now),
            self.items.len()
        );
        for p in &self.items {
            t.push_str(&Self::line(p));
            t.push_str("\r\n");
        }
        t
    }

    /// Uloží úplný seznam do `<dir>\linuxfs-errors-<tag>.txt` (UTF-8 s BOM, aby ho Poznámkový blok přečetl správně).
    /// Soubor se přepisuje: patří vždy poslední operaci daného druhu (`tag`: `list`, `unpack`).
    pub fn write_file_in(&self, dir: &Path, tag: &str, title: &str, archive: &str) -> Option<PathBuf> {
        let path = dir.join(format!("linuxfs-errors-{tag}.txt"));
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(self.file_text(title, archive).as_bytes());
        fs::write(&path, bytes).ok().map(|_| path)
    }

    /// Jako [`ProblemLog::write_file_in`] do adresáře pro dočasné soubory.
    pub fn write_file(&self, tag: &str, title: &str, archive: &str) -> Option<PathBuf> {
        self.write_file_in(&std::env::temp_dir(), tag, title, archive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_file() {
        let mut l = ProblemLog::default();
        assert!(l.is_empty());
        for i in 0..11 {
            l.add(format!("dir\\f{i}"), "boom");
        }
        l.add_warnings("", vec!["root problem".to_string()]);
        assert_eq!(l.len(), 12);
        let w = l.window_text("could not be read", Some(Path::new("C:\\t\\x.txt")));
        assert!(w.starts_with("12 problems could not be read"), "{w}");
        assert!(w.contains("dir\\f0: boom") && !w.contains("dir\\f9"), "{w}");
        assert!(w.contains("... and 4 more") && w.contains("Full list: C:\\t\\x.txt"), "{w}");
        let one = {
            let mut o = ProblemLog::default();
            o.add("a", "b");
            o
        };
        assert!(one.window_text("x", None).starts_with("1 problem x"));

        let dir = std::env::temp_dir().join(format!("linuxfs-report-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = l.write_file_in(&dir, "list", "listing", "disk.img").unwrap();
        let bytes = fs::read(&p).unwrap();
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        assert!(text.contains("Problems: 12") && text.contains("dir\\f10: boom\r\n"), "{text}");
        assert!(text.contains("\\: root problem"), "{text}");
        let _ = fs::remove_dir_all(dir);
    }
}
