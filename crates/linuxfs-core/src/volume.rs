//! Jednotný pohled na jeden svazek Linuxu (btrfs, ext2/3/4, squashfs), aby pluginy nemusely řešit, o který souborový systém jde.
//!
//! Položky se adresují číslem ([`NodeId`]: strom + inode), ne cestou: názvy souborů v Linuxu jsou libovolné bajty
//! a po převodu do Unicode (a po očistě pro Windows) by se podle nich položka nedala spolehlivě najít.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::btrfs::{BtrfsVolume, SubvolSelect};
use crate::ext::ExtVolume;
use crate::squashfs::SquashVolume;
use crate::detect::{FsKind, Part};
use crate::source::{DiskSource, Slice};

/// Virtuální soubor v kořeni svazku btrfs s popisem souborového systému a seznamem podsvazků (u ext viz [`info_name_for`]).
pub const INFO_NAME: &str = "!BTRFS-INFO.txt";

/// Jméno virtuálního informačního souboru podle druhu souborového systému.
pub fn info_name_for(kind: FsKind) -> &'static str {
    match kind {
        FsKind::Btrfs => INFO_NAME,
        FsKind::Ext2 => "!EXT2-INFO.txt",
        FsKind::Ext3 => "!EXT3-INFO.txt",
        FsKind::Ext4 => "!EXT4-INFO.txt",
        FsKind::Squashfs => "!SQUASHFS-INFO.txt",
    }
}

/// Výpis adresáře: položky a upozornění na to, co se nepodařilo přečíst (nečitelné položky se nevynechávají potichu).
#[derive(Debug, Default)]
pub struct Listing {
    pub nodes: Vec<Node>,
    /// Texty bez cesty adresáře (tu zná volající).
    pub warnings: Vec<String>,
}

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
    Ext(Box<ExtVolume>),
    Squash(Box<SquashVolume>),
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
            Fs::Ext(v) => v.root(),
            Fs::Squash(v) => v.root(),
        }
    }

    /// Uzel virtuálního informačního souboru.
    pub fn info_node(&self) -> Node {
        Node {
            name: info_name_for(self.kind).to_string(),
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
            Fs::Ext(v) => v.info_text(),
            Fs::Squash(v) => v.info_text(),
        }
    }

    /// Položky adresáře; kořen navíc obsahuje virtuální [`INFO_NAME`].
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Vec<Node>, VolumeError> {
        Ok(self.list_directory_ex(dir)?.nodes)
    }

    /// Jako [`LinuxVolume::list_directory`], ale s upozorněními na položky, které se nepodařilo přečíst.
    pub fn list_directory_ex(&mut self, dir: NodeId) -> Result<Listing, VolumeError> {
        let mut l = match &mut self.fs {
            Fs::Btrfs(v) => {
                let mut warnings = Vec::new();
                let nodes = v.list_directory_with_warnings(dir, &mut warnings)?;
                Listing { nodes, warnings }
            }
            Fs::Ext(v) => v.list_directory(dir)?,
            Fs::Squash(v) => v.list_directory(dir)?,
        };
        if dir == self.root() {
            l.nodes.push(self.info_node());
        }
        Ok(l)
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
            Fs::Ext(v) => v.read_file_to(id, out),
            Fs::Squash(v) => v.read_file_to(id, out),
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
    match part.kind {
        FsKind::Ext2 | FsKind::Ext3 | FsKind::Ext4 => {
            let v = ExtVolume::open(src, part.start, part.len)?;
            let name = v.label().to_string();
            Ok(LinuxVolume {
                kind: v.kind(),
                fs: Fs::Ext(Box::new(v)),
                name,
            })
        }
        FsKind::Squashfs => {
            let v = SquashVolume::open(src, part.start, part.len)?;
            let name = v.label().to_string();
            Ok(LinuxVolume {
                kind: v.kind(),
                fs: Fs::Squash(Box::new(v)),
                name,
            })
        }
        FsKind::Btrfs => {
            let slice = Slice::new(src.clone(), part.start, part.len);
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

/// Výsledek otevření všech oddílů: otevřené svazky a hlášení o těch, které se otevřít nepodařilo.
pub struct OpenedSet {
    pub volumes: Vec<OpenedVolume>,
    /// `(označení oddílu, text chyby)` pro oddíly, které se přeskočily.
    pub failed: Vec<(String, String)>,
}

/// Otevře všechny svazky všech oddílů a každému dá jméno složky (`Partition 2 - jmenovka`). Oddíly, které nejdou
/// otevřít, se přeskočí a vrátí se v [`OpenedSet::failed`]; chyba se vrátí, jen když nejde otevřít vůbec nic.
pub fn open_all_report(
    src: &Arc<Mutex<DiskSource>>,
    parts: &[Part],
    select: &SubvolSelect,
) -> Result<OpenedSet, VolumeError> {
    let mut volumes = Vec::new();
    let mut failed = Vec::new();
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
                volumes.push(OpenedVolume { label, vol });
            }
            Err(e) => {
                failed.push((
                    format!("Partition {} ({})", i + 1, p.kind.name()),
                    e.to_string(),
                ));
                first_err.get_or_insert(e);
            }
        }
    }
    if volumes.is_empty() {
        return Err(first_err.unwrap_or_else(|| VolumeError::Format("no volume".into())));
    }
    Ok(OpenedSet { volumes, failed })
}

/// Jako [`open_all_report`], ale bez hlášení o přeskočených oddílech.
pub fn open_all(
    src: &Arc<Mutex<DiskSource>>,
    parts: &[Part],
    select: &SubvolSelect,
) -> Result<Vec<OpenedVolume>, VolumeError> {
    Ok(open_all_report(src, parts, select)?.volumes)
}
