//! ext2 / ext3 / ext4 jen pro čtení nad crate `ext4-view` (MIT OR Apache-2.0).
//!
//! Crate `ext4-view` adresuje položky cestou (bajty), ne číslem inode, proto si tu vedeme tabulku `NodeId.ino` -> cesta:
//! kořen má číslo 0, každá další položka, kterou výpis vrátil, dostane další číslo. Čísla platí jen po dobu života
//! svazku. Hard linky mají víc cest, tedy víc čísel – pro čtení to nevadí.
//!
//! Žurnál (ext3/ext4) se nikdy nepřehrává na disk: `ext4-view` ho jen namapuje v paměti, takže i nečistě odpojený
//! svazek se zobrazí tak, jak by vypadal po obnově, a na zdroji se nic nemění.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ext4_view::{Ext4, Ext4Error, Ext4Read, FileType, PathBuf as ExtPath};

use crate::detect::FsKind;
use crate::source::DiskSource;
use crate::util;
use crate::volume::{Listing, Node, NodeId, NodeKind, VolumeError};

/// Posun superbloku ext od začátku svazku (pevně 1024 B bez ohledu na velikost bloku).
pub const SUPER_OFFSET: u64 = 1024;
const SUPER_LEN: usize = 1024;
const MAGIC: u16 = 0xEF53;

// ---------------------------------------------------------------------------------------------------------------------
// superblok
// ---------------------------------------------------------------------------------------------------------------------

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Pole superbloku, která pluginy zobrazují a podle kterých se pozná ext2 / ext3 / ext4.
#[derive(Debug, Clone)]
pub struct ExtSuper {
    pub inodes: u32,
    pub blocks: u64,
    pub free_blocks: u64,
    pub free_inodes: u32,
    pub block_size: u64,
    pub state: u16,
    pub rev: u32,
    pub compat: u32,
    pub incompat: u32,
    pub ro_compat: u32,
    pub uuid: [u8; 16],
    pub label: Vec<u8>,
    pub last_mounted: Vec<u8>,
    pub mkfs_time: u32,
    pub write_time: u32,
    pub inode_size: u32,
}

const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_JOURNAL_DEV: u32 = 0x8;
const INCOMPAT_64BIT: u32 = 0x80;
const COMPAT_HAS_JOURNAL: u32 = 0x4;

impl ExtSuper {
    /// Přečte superblok z 1024 bajtů; `None`, když to není (věrohodný) superblok ext.
    pub fn parse(b: &[u8]) -> Option<ExtSuper> {
        if b.len() < SUPER_LEN || le16(b, 0x38) != MAGIC {
            return None;
        }
        let log_bs = le32(b, 0x18);
        let rev = le32(b, 0x4C);
        let (compat, incompat, ro_compat) = if rev >= 1 {
            (le32(b, 0x5C), le32(b, 0x60), le32(b, 0x64))
        } else {
            (0, 0, 0)
        };
        // Rozumné hodnoty (náhodná shoda dvou bajtů magie nestačí): blok 1 KiB..64 KiB, skupiny a inody nenulové.
        if log_bs > 6 || rev > 1 || le32(b, 0x20) == 0 || le32(b, 0x28) == 0 || le32(b, 0x0) == 0 {
            return None;
        }
        if incompat & INCOMPAT_JOURNAL_DEV != 0 {
            return None; // samostatné zařízení se žurnálem, ne souborový systém
        }
        let hi = |o: usize| {
            if incompat & INCOMPAT_64BIT != 0 {
                (le32(b, o) as u64) << 32
            } else {
                0
            }
        };
        let trim = |s: &[u8]| s.split(|&c| c == 0).next().unwrap_or(&[]).to_vec();
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&b[0x68..0x78]);
        Some(ExtSuper {
            inodes: le32(b, 0x0),
            blocks: le32(b, 0x4) as u64 | hi(0x150),
            free_blocks: le32(b, 0xC) as u64 | hi(0x158),
            free_inodes: le32(b, 0x10),
            block_size: 1024u64 << log_bs,
            state: le16(b, 0x3A),
            rev,
            compat,
            incompat,
            ro_compat,
            uuid,
            label: trim(&b[0x78..0x88]),
            last_mounted: trim(&b[0x88..0xC8]),
            mkfs_time: if rev >= 1 { le32(b, 0x108) } else { 0 },
            write_time: le32(b, 0x30),
            inode_size: if rev >= 1 { le16(b, 0x58) as u32 } else { 128 },
        })
    }

    /// ext2 / ext3 / ext4 podle příznaků, stejně jako to rozlišuje jádro: ext3 je ext4 bez novějších vlastností.
    pub fn kind(&self) -> FsKind {
        // vlastnosti, které existovaly už v ext2 / ext3
        const COMPAT_OLD: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20;
        const INCOMPAT_OLD: u32 = 0x2 | 0x4 | 0x10;
        const RO_OLD: u32 = 0x1 | 0x2 | 0x4;
        if self.compat & !COMPAT_OLD != 0
            || self.incompat & !INCOMPAT_OLD != 0
            || self.ro_compat & !RO_OLD != 0
        {
            FsKind::Ext4
        } else if self.compat & COMPAT_HAS_JOURNAL != 0 {
            FsKind::Ext3
        } else {
            FsKind::Ext2
        }
    }

    pub fn needs_recovery(&self) -> bool {
        self.incompat & INCOMPAT_RECOVER != 0
    }

    pub fn has_journal(&self) -> bool {
        self.compat & COMPAT_HAS_JOURNAL != 0
    }
}

/// Začíná na `start` ext2/3/4? Vrací i rozlišený druh.
pub fn probe(src: &mut DiskSource, start: u64, len: u64) -> Option<FsKind> {
    if len < SUPER_OFFSET + SUPER_LEN as u64 {
        return None;
    }
    let mut b = [0u8; SUPER_LEN];
    src.read_exact_at(start.checked_add(SUPER_OFFSET)?, &mut b)
        .ok()?;
    Some(ExtSuper::parse(&b)?.kind())
}

fn feature_names(s: &ExtSuper) -> String {
    const COMPAT: [(u32, &str); 8] = [
        (0x4, "has_journal"),
        (0x8, "ext_attr"),
        (0x10, "resize_inode"),
        (0x20, "dir_index"),
        (0x40, "sparse_super2"),
        (0x80, "fast_commit"),
        (0x100, "stable_inodes"),
        (0x2, "imagic_inodes"),
    ];
    const INCOMPAT: [(u32, &str); 15] = [
        (0x1, "compression"),
        (0x2, "filetype"),
        (0x4, "needs_recovery"),
        (0x10, "meta_bg"),
        (0x40, "extents"),
        (0x80, "64bit"),
        (0x100, "mmp"),
        (0x200, "flex_bg"),
        (0x400, "ea_inode"),
        (0x1000, "dirdata"),
        (0x2000, "metadata_csum_seed"),
        (0x4000, "large_dir"),
        (0x8000, "inline_data"),
        (0x10000, "encrypt"),
        (0x20000, "casefold"),
    ];
    const RO: [(u32, &str); 12] = [
        (0x1, "sparse_super"),
        (0x2, "large_file"),
        (0x8, "huge_file"),
        (0x10, "gdt_csum"),
        (0x20, "dir_nlink"),
        (0x40, "extra_isize"),
        (0x100, "quota"),
        (0x200, "bigalloc"),
        (0x400, "metadata_csum"),
        (0x1000, "read-only"),
        (0x2000, "project"),
        (0x8000, "verity"),
    ];
    let mut v: Vec<&str> = Vec::new();
    v.extend(COMPAT.iter().filter(|(b, _)| s.compat & b != 0).map(|x| x.1));
    v.extend(INCOMPAT.iter().filter(|(b, _)| s.incompat & b != 0).map(|x| x.1));
    v.extend(RO.iter().filter(|(b, _)| s.ro_compat & b != 0).map(|x| x.1));
    v.join(" ")
}

fn uuid_text(u: &[u8; 16]) -> String {
    let h: String = u.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn info_text(kind: FsKind, s: &ExtSuper) -> String {
    let size = s.blocks.saturating_mul(s.block_size);
    let free = s.free_blocks.saturating_mul(s.block_size);
    let mut t = String::new();
    t.push_str(&format!("{} file system (read-only view)\r\n\r\n", kind.name()));
    t.push_str(&format!("Label:        {}\r\n", String::from_utf8_lossy(&s.label)));
    t.push_str(&format!("UUID:         {}\r\n", uuid_text(&s.uuid)));
    t.push_str(&format!("Block size:   {} B\r\n", s.block_size));
    t.push_str(&format!("Size:         {} B ({} blocks)\r\n", size, s.blocks));
    t.push_str(&format!("Free:         {} B ({} blocks)\r\n", free, s.free_blocks));
    t.push_str(&format!("Inodes:       {} ({} free), inode size {} B\r\n", s.inodes, s.free_inodes, s.inode_size));
    if s.mkfs_time != 0 {
        t.push_str(&format!("Created:      {} UTC\r\n", util::format_utc(s.mkfs_time as i64)));
    }
    if s.write_time != 0 {
        t.push_str(&format!("Last write:   {} UTC\r\n", util::format_utc(s.write_time as i64)));
    }
    if !s.last_mounted.is_empty() {
        t.push_str(&format!("Last mounted: {}\r\n", String::from_utf8_lossy(&s.last_mounted)));
    }
    t.push_str(&format!("State:        {}\r\n", match (s.state & 1 != 0, s.state & 2 != 0) {
        (true, false) => "clean",
        (_, true) => "errors detected",
        (false, false) => "not cleanly unmounted",
    }));
    t.push_str(&format!("Features:     {}\r\n", feature_names(s)));
    if s.needs_recovery() {
        t.push_str("\r\nThe journal has not been replayed (the file system was not cleanly unmounted). Nothing is written to the\r\n");
        t.push_str("source: the plugin only maps the journal in memory, so you see the state a mount would produce after recovery.\r\n");
    } else if s.has_journal() {
        t.push_str("\r\nThe journal is clean.\r\n");
    }
    t
}

// ---------------------------------------------------------------------------------------------------------------------
// čtení zdroje pro ext4-view
// ---------------------------------------------------------------------------------------------------------------------

struct SrcReader {
    src: Arc<Mutex<DiskSource>>,
    start: u64,
    len: u64,
}

impl Ext4Read for SrcReader {
    fn read(
        &mut self,
        start_byte: u64,
        dst: &mut [u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        let end = start_byte.checked_add(dst.len() as u64);
        if end.is_none_or(|e| e > self.len) {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("read beyond the end of the partition (offset {start_byte})"),
            )));
        }
        let mut src = self.src.lock().unwrap_or_else(|e| e.into_inner());
        src.read_exact_at(self.start + start_byte, dst)
            .map_err(|e| Box::new(e) as _)
    }
}

fn map_err(e: Ext4Error) -> VolumeError {
    match e {
        Ext4Error::Io(b) => match b.downcast::<io::Error>() {
            Ok(io) => VolumeError::Io(*io),
            Err(other) => VolumeError::Io(io::Error::other(other.to_string())),
        },
        Ext4Error::Incompatible(i) => VolumeError::Unsupported(format!("unsupported ext feature: {i}")),
        Ext4Error::Encrypted => VolumeError::Unsupported("encrypted (fscrypt) item".into()),
        Ext4Error::NotFound => VolumeError::NotFound("ext item".into()),
        other => VolumeError::Format(other.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// svazek
// ---------------------------------------------------------------------------------------------------------------------

pub struct ExtVolume {
    fs: Ext4,
    kind: FsKind,
    label: String,
    info: String,
    /// `NodeId.ino` -> cesta v souborovém systému (index 0 = kořen).
    paths: Vec<ExtPath>,
    index: HashMap<Vec<u8>, u64>,
}

impl ExtVolume {
    pub fn open(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<Self, VolumeError> {
        let mut b = [0u8; SUPER_LEN];
        {
            let mut s = src.lock().unwrap_or_else(|e| e.into_inner());
            s.read_exact_at(start + SUPER_OFFSET, &mut b)?;
        }
        let sup = ExtSuper::parse(&b)
            .ok_or_else(|| VolumeError::Format("not an ext2/3/4 superblock".into()))?;
        let kind = sup.kind();
        let reader = SrcReader {
            src: src.clone(),
            start,
            len,
        };
        let fs = Ext4::load(Box::new(reader)).map_err(map_err)?;
        let root = ExtPath::new("/");
        let mut index = HashMap::new();
        index.insert(root.as_ref().to_vec(), 0);
        Ok(ExtVolume {
            fs,
            kind,
            label: String::from_utf8_lossy(&sup.label).into_owned(),
            info: info_text(kind, &sup),
            paths: vec![root],
            index,
        })
    }

    pub fn kind(&self) -> FsKind {
        self.kind
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn info_text(&self) -> &str {
        &self.info
    }
    pub fn root(&self) -> NodeId {
        NodeId { tree: 0, ino: 0 }
    }

    fn path_of(&self, id: NodeId) -> Result<&ExtPath, VolumeError> {
        usize::try_from(id.ino)
            .ok()
            .filter(|_| id.tree == 0)
            .and_then(|i| self.paths.get(i))
            .ok_or_else(|| VolumeError::NotFound(format!("ext item #{}", id.ino)))
    }

    fn intern(&mut self, p: ExtPath) -> u64 {
        let key = p.as_ref().to_vec();
        if let Some(&n) = self.index.get(&key) {
            return n;
        }
        let n = self.paths.len() as u64;
        self.paths.push(p);
        self.index.insert(key, n);
        n
    }

    /// Položky adresáře. Nečitelná položka (poškozený záznam nebo inode) se přeskočí / zobrazí bez údajů a hlásí se
    /// v `warnings`; adresář jako celek se vrací, dokud jde projít.
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Listing, VolumeError> {
        let path = self.path_of(dir)?.clone();
        let rd = self.fs.read_dir(&path).map_err(map_err)?;
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    // poškozený blok adresáře: jeho položky chybí, výpis pokračuje dalším blokem (vendorovaný patch)
                    warnings.push(format!("some entries of the directory are missing, a block is unreadable: {}", map_err(e)));
                    continue;
                }
            };
            let raw = entry.file_name();
            let raw: &[u8] = raw.as_ref();
            if raw == b"." || raw == b".." {
                continue;
            }
            let name = String::from_utf8_lossy(raw).into_owned();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(e) => {
                    warnings.push(format!("'{name}': {}", map_err(e)));
                    continue;
                }
            };
            let kind = match ft {
                FileType::Regular => NodeKind::File,
                FileType::Directory => NodeKind::Dir,
                FileType::Symlink => NodeKind::Symlink,
                _ => continue, // fifo, socket, zařízení: ve Windows nemají smysl
            };
            let (size, mtime) = match entry.metadata() {
                Ok(m) => (
                    if kind == NodeKind::Dir { 0 } else { m.len() },
                    Some(m.modified().seconds()),
                ),
                Err(e) => {
                    // položku ukážeme, i když nemá čitelný inode; čtení pak případně selže a ohlásí se zvlášť
                    warnings.push(format!("'{name}': metadata unreadable: {}", map_err(e)));
                    (0, None)
                }
            };
            let ino = self.intern(entry.path());
            out.push(Node {
                name,
                kind,
                size,
                mtime,
                id: NodeId { tree: 0, ino },
            });
        }
        Ok(Listing {
            nodes: out,
            warnings,
        })
    }

    /// Pošle obsah souboru (nebo cíl symlinku) do `out`.
    pub fn read_file_to(&mut self, id: NodeId, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let path = self.path_of(id)?.clone();
        let meta = self.fs.symlink_metadata(&path).map_err(map_err)?;
        if meta.is_dir() {
            return Err(VolumeError::Format("a directory has no contents to read".into()));
        }
        if meta.is_symlink() {
            let target = self.fs.read_link(&path).map_err(map_err)?;
            let t: &[u8] = target.as_ref();
            out.write_all(t)?;
            return Ok(t.len() as u64);
        }
        let mut f = self.fs.open(&path).map_err(map_err)?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = f.read_bytes(&mut buf).map_err(map_err)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
        Ok(total)
    }
}
