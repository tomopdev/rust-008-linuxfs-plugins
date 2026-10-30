//! Jednotný pohled na jeden svazek Linuxu (zatím btrfs), aby pluginy nemusely řešit, o který souborový systém jde.
//!
//! Položky se adresují číslem ([`NodeId`]: strom + inode), ne cestou: názvy souborů v Linuxu jsou libovolné bajty
//! a po převodu do Unicode (a po očistě pro Windows) by se podle nich položka nedala spolehlivě najít.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::btrfs::{BtrfsVolume, SubvolSelect};
use crate::detect::{FsKind, Part};
use crate::source::{DiskSource, Slice};

/// Virtuální soubor v kořeni svazku s popisem souborového systému a seznamem podsvazků.
pub const INFO_NAME: &str = "!BTRFS-INFO.txt";

/// Adresa položky ve svazku: strom (u btrfs podsvazek) a číslo inode v něm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    pub tree: u64,
    pub ino: u64,
}

/// Adresa virtuálního souboru [`INFO_NAME`] (strom `u64::MAX` v žádném svazku neexistuje).
pub const INFO_ID: NodeId = NodeId {
    tree: u64::MAX,
    ino: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    /// Symbolický odkaz; zobrazuje se jako malý soubor s cílem odkazu.
    Symlink,
}

/// Jedna položka adresáře nezávislá na souborovém systému.
#[derive(Debug, Clone)]
pub struct Node {
    /// Jméno (neplatné UTF-8 je nahrazeno U+FFFD).
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
    /// Čas změny, unix sekundy (UTC).
    pub mtime: Option<i64>,
    pub id: NodeId,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

#[derive(Debug)]
pub enum VolumeError {
    Io(io::Error),
    /// Poškozené struktury a vše, co není prostý I/O.
    Format(String),
    NotFound(String),
    /// Vlastnost, kterou čtečka neumí (šifrování, zoned...).
    Unsupported(String),
}

impl VolumeError {
    pub fn is_io(&self) -> bool {
        matches!(self, VolumeError::Io(_))
    }
}

impl fmt::Display for VolumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VolumeError::Io(e) => write!(f, "I/O error: {e}"),
            VolumeError::Format(m) | VolumeError::Unsupported(m) => f.write_str(m),
            VolumeError::NotFound(p) => write!(f, "not found: {p}"),
        }
    }
}

impl std::error::Error for VolumeError {}

impl From<io::Error> for VolumeError {
    fn from(e: io::Error) -> Self {
        VolumeError::Io(e)
    }
}

enum Fs {
    Btrfs(Box<BtrfsVolume>),
}

pub struct LinuxVolume {
    fs: Fs,
    kind: FsKind,
    /// Jmenovka souborového systému (může být prázdná).
    name: String,
}

impl LinuxVolume {
    pub fn kind(&self) -> FsKind {
        self.kind
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Kořenový adresář (u btrfs kořen zvoleného podsvazku).
    pub fn root(&self) -> NodeId {
        match &self.fs {
            Fs::Btrfs(v) => v.root(),
        }
    }

    /// Uzel virtuálního informačního souboru.
    pub fn info_node(&self) -> Node {
        Node {
            name: INFO_NAME.to_string(),
            kind: NodeKind::File,
            size: self.info_text().len() as u64,
            mtime: None,
            id: INFO_ID,
        }
    }

    /// Text informačního souboru (pro testy a pluginy).
    pub fn info_text_for_tests(&self) -> &str {
        self.info_text()
    }

    fn info_text(&self) -> &str {
        match &self.fs {
            Fs::Btrfs(v) => v.info_text(),
        }
    }

    /// Položky adresáře; kořen navíc obsahuje virtuální [`INFO_NAME`].
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Vec<Node>, VolumeError> {
        let mut nodes = match &mut self.fs {
            Fs::Btrfs(v) => v.list_directory(dir)?,
        };
        if dir == self.root() {
            nodes.push(self.info_node());
        }
        Ok(nodes)
    }

    /// Pošle obsah souboru do `out` (virtuální soubor se vygeneruje).
    pub fn read_file_to(&mut self, id: NodeId, out: &mut dyn Write) -> Result<u64, VolumeError> {
        if id == INFO_ID {
            let t = self.info_text();
            out.write_all(t.as_bytes())?;
            return Ok(t.len() as u64);
        }
        match &mut self.fs {
            Fs::Btrfs(v) => v.read_file_to(id, out),
        }
    }
}

/// Svazek a jméno složky, kterou dostane, když zdroj obsahuje víc svazků.
pub struct OpenedVolume {
    pub label: String,
    pub vol: LinuxVolume,
}

/// Otevře svazek v jednom oddílu.
pub fn open_part(
    src: &Arc<Mutex<DiskSource>>,
    part: &Part,
    select: &SubvolSelect,
) -> Result<LinuxVolume, VolumeError> {
    let slice = Slice::new(src.clone(), part.start, part.len);
    match part.kind {
        FsKind::Btrfs => {
            let v = BtrfsVolume::open(slice, select)?;
            let name = v.label().to_string();
            Ok(LinuxVolume {
                fs: Fs::Btrfs(Box::new(v)),
                kind: FsKind::Btrfs,
                name,
            })
        }
    }
}

/// Otevře všechny svazky všech oddílů a každému dá jméno složky (`Partition 2 - jmenovka`). Oddíly, které nejdou
/// otevřít, se přeskočí; první chyba se vrátí, jen když nejde otevřít vůbec nic.
pub fn open_all(
    src: &Arc<Mutex<DiskSource>>,
    parts: &[Part],
    select: &SubvolSelect,
) -> Result<Vec<OpenedVolume>, VolumeError> {
    let mut out = Vec::new();
    let mut first_err = None;
    for (i, p) in parts.iter().enumerate() {
        match open_part(src, p, select) {
            Ok(vol) => {
                let tag = if p.label.is_empty() {
                    vol.name().to_string()
                } else {
                    p.label.clone()
                };
                let label = if tag.is_empty() {
                    format!("Partition {}", i + 1)
                } else {
                    format!("Partition {} - {}", i + 1, tag)
                };
                out.push(OpenedVolume { label, vol });
            }
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    if out.is_empty() {
        return Err(first_err.unwrap_or_else(|| VolumeError::Format("no volume".into())));
    }
    Ok(out)
}
