//! EROFS (Enhanced Read-Only File System) jen pro čtení nad crate `rust-fs-erofs` (MIT, čistý Rust; compact i extended inody,
//! FLAT_PLAIN, FLAT_INLINE, chunkové soubory, komprese LZ4 / LZMA / DEFLATE / ZSTD vč. compacted-2B, ztailpacking, fragmentů,
//! dedupe a big_pcluster).
//!
//! Položky se adresují skutečným číslem inode EROFS (`NodeId.ino` = NID), takže se nevede žádná tabulka cest a číslo platí
//! pro celý obraz. Hard linky mají stejné NID. EROFS má jen jeden strom, proto je `NodeId.tree` vždy 0.
//!
//! Superblok leží vždy na +1024 B (prvních 1024 B je volných pro zavaděč) a je chráněn CRC32C, pokud obraz nese
//! `sb_chksum`; crate při otevření nesouhlasící součet odmítne. Data souborů kontrolní součty nemají. Compact inod nemá vlastní čas
//! změny, proto se (stejně jako v jádře) ukáže čas vytvoření obrazu ze superbloku.
//!
//! Obraz s dalšími zařízeními (`--blobdev`, tabulka zařízení) a obraz s 48bitovým adresováním nebo „metabox“ se odmítne
//! s vysvětlením (viz [`open`](ErofsVolume::open)): místo tichého čtení nesprávných dat.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use fs_core::Error as BlockError;
use fs_erofs::superblock::{
    EROFS_FEATURE_INCOMPAT_SUPPORTED, EROFS_SUPER_BLOCK_SIZE, EROFS_SUPER_MAGIC_V1, EROFS_SUPER_OFFSET, MAX_BLKSZBITS, MIN_BLKSZBITS,
};
use fs_erofs::{Error as ErofsError, Filesystem, Inode, InodeVersion, Superblock};

use crate::blockdev::SrcDevice;
use crate::detect::FsKind;
use crate::source::DiskSource;
use crate::util;
use crate::volume::{Listing, Node, NodeId, NodeKind, VolumeError};

/// Čtení souboru po kusech této velikosti (každé volání čtečky znovu sestaví index komprimovaného souboru, proto ne po malých dávkách).
const READ_CHUNK: usize = 4 * 1024 * 1024;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Věrohodný superblok EROFS? Magie sama nestačí (čtyři bajty může mít i jiný soubor), proto se ověřuje i rozumná velikost
/// bloku (9..=16 bitů) a nenulový počet bloků.
///
/// Záměrně se NEověřuje CRC ani podporované vlastnosti (`dirblkbits`, bity `feature_incompat`): obraz, který crate nezvládne
/// nebo který má vadný součet, se má poznat jako EROFS a při otevření ohlásit skutečný důvod, ne tiše zmizet z nabídky.
fn plausible_super(b: &[u8]) -> bool {
    if b.len() < EROFS_SUPER_BLOCK_SIZE || le32(b, 0x00) != EROFS_SUPER_MAGIC_V1 {
        return false;
    }
    (MIN_BLKSZBITS..=MAX_BLKSZBITS).contains(&b[0x0C]) && le32(b, 0x24) != 0 // blocks
}

/// Začíná na `start` EROFS?
pub fn probe(src: &mut DiskSource, start: u64, len: u64) -> bool {
    if len < EROFS_SUPER_OFFSET + EROFS_SUPER_BLOCK_SIZE as u64 {
        return false;
    }
    let Some(at) = start.checked_add(EROFS_SUPER_OFFSET) else {
        return false;
    };
    let mut b = [0u8; EROFS_SUPER_BLOCK_SIZE];
    src.read_exact_at(at, &mut b).is_ok() && plausible_super(&b)
}

fn map_err(e: ErofsError) -> VolumeError {
    match e {
        // chyba čtení zdroje: zůstává I/O chybou, pluginy ji hlásí jako chybu čtení
        ErofsError::Block(BlockError::Io(io)) => VolumeError::Io(io),
        ErofsError::Block(other) => VolumeError::Io(io::Error::other(format!("{other:?}"))),
        ErofsError::NotFound => VolumeError::NotFound("EROFS item".into()),
        // vlastnost, kterou crate neumí (48bit, metabox, neznámý bit; neznámý kompresní algoritmus)
        ErofsError::BadSuperblock(m) if m.contains("not implemented") || m.contains("does not implement") => {
            VolumeError::Unsupported(format!("unsupported EROFS feature: {m}"))
        }
        e @ ErofsError::UnsupportedLayout(_) => VolumeError::Unsupported(e.to_string()),
        // poškozené struktury (inod, adresář, superblok, součet): popis crate je srozumitelný, jen se doplní „EROFS“
        ErofsError::NotErofs => VolumeError::Format("not an EROFS image".into()),
        ErofsError::BadSuperblock(m) => VolumeError::Format(format!("damaged EROFS superblock: {m}")),
        ErofsError::BadInode(m) => VolumeError::Format(format!("damaged EROFS inode: {m}")),
        ErofsError::BadDirent(m) => VolumeError::Format(format!("damaged EROFS directory: {m}")),
        ErofsError::BadXattr(m) => VolumeError::Format(format!("damaged EROFS xattr: {m}")),
        other => VolumeError::Format(other.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// informační soubor
// ---------------------------------------------------------------------------------------------------------------------

fn uuid_text(u: &[u8; 16]) -> String {
    let h: String = u.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Názvy zapnutých vlastností (`feature_compat` a `feature_incompat`). Bity 0x2 a 0x20 v `feature_incompat` mají v jádře dva
/// významy (`compr_cfgs` = `big_pcluster`, `fragments` = `dedupe`), které se v superbloku nedají rozlišit.
fn feature_names(compat: u32, incompat: u32) -> String {
    const COMPAT: [(u32, &str); 3] = [(0x1, "sb_chksum"), (0x2, "mtime"), (0x4, "xattr_filter")];
    const INCOMPAT: [(u32, &str); 9] = [
        (0x001, "zero_padding"),
        (0x002, "compr_cfgs/big_pcluster"),
        (0x004, "chunked_file"),
        (0x008, "device_table"),
        (0x010, "ztailpacking"),
        (0x020, "fragments/dedupe"),
        (0x040, "xattr_prefixes"),
        (0x080, "48bit"),
        (0x100, "metabox"),
    ];
    COMPAT
        .iter()
        .filter(|(b, _)| compat & b != 0)
        .chain(INCOMPAT.iter().filter(|(b, _)| incompat & b != 0))
        .map(|x| x.1)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Kompresory, které obraz ohlašuje v `COMPR_CFGS` (bez nich je obraz nekomprimovaný nebo používá starý LZ4).
fn compressor_names(fs: &Filesystem) -> String {
    let Some(c) = fs.compr_cfgs() else {
        return "none declared (uncompressed image, or legacy LZ4)".into();
    };
    let mut v: Vec<String> = Vec::new();
    if c.lz4.is_some() {
        v.push("lz4".into());
    }
    if let Some(l) = c.lzma {
        v.push(format!("lzma (dictionary {} KiB)", l.dict_size / 1024));
    }
    if c.deflate.is_some() {
        v.push("deflate".into());
    }
    if let Some(z) = c.zstd {
        // windowlog = ZSTD_windowLog - 10, takže okno má 1 << (windowlog + 10) B = 1 << windowlog KiB (hodnota z obrazu, proto bez přetečení)
        match 1u64.checked_shl(u32::from(z.windowlog)) {
            Some(kib) => v.push(format!("zstd (window {kib} KiB)")),
            None => v.push("zstd".into()),
        }
    }
    if v.is_empty() { "none declared".into() } else { v.join(", ") }
}

fn info_text(fs: &Filesystem) -> String {
    let sb = fs.superblock();
    let bs = sb.block_size();
    let mut t = String::new();
    t.push_str("erofs file system (read-only view)\r\n\r\n");
    t.push_str(&format!("Label:        {}\r\n", sb.volume_name_str()));
    t.push_str(&format!("UUID:         {}\r\n", uuid_text(&sb.uuid)));
    t.push_str(&format!("Block size:   {bs} B\r\n"));
    t.push_str(&format!("Size:         {} B ({} blocks)\r\n", u64::from(sb.blocks).saturating_mul(bs), sb.blocks));
    t.push_str(&format!("Inodes:       {}\r\n", sb.inos));
    t.push_str(&format!("Created:      {} UTC\r\n", util::format_utc(i64::try_from(sb.build_time).unwrap_or(0))));
    t.push_str(&format!("Compressors:  {}\r\n", compressor_names(fs)));
    t.push_str(&format!("Features:     {}\r\n", feature_names(sb.feature_compat, sb.feature_incompat)));
    if sb.feature_compat & 0x1 != 0 {
        t.push_str("\r\nThe superblock checksum (CRC32C) was verified when the image was opened. File data has no checksums.\r\n");
    } else {
        t.push_str("\r\nThe image has no checksums. A damaged image shows up only where a structure check or decompression fails.\r\n");
    }
    t.push_str("Extended attributes and ACLs are not shown. Special files (devices, FIFOs, sockets) are not shown.\r\n");
    t
}

// ---------------------------------------------------------------------------------------------------------------------
// svazek
// ---------------------------------------------------------------------------------------------------------------------

pub struct ErofsVolume {
    fs: Filesystem,
    label: String,
    info: String,
    root: u64,
}

/// Čas změny položky: extended inod ho má, compact inod ne, a tam platí čas vytvoření obrazu (jako v jádře).
fn mtime_of(sb: &Superblock, inode: &Inode) -> Option<i64> {
    let secs = match inode.format.version {
        InodeVersion::Extended => inode.mtime,
        InodeVersion::Compact => sb.build_time,
    };
    i64::try_from(secs).ok()
}

impl ErofsVolume {
    /// Otevře obraz. Odmítne s vysvětlením:
    /// * obraz s dalšími zařízeními (`--blobdev`, tabulka zařízení): data na nich nemáme, takže by se část souborů četla špatně nebo vůbec,
    /// * obraz s vlastností, kterou crate neumí (48bitové adresy, „metabox“, neznámý bit `feature_incompat`).
    pub fn open(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<Self, VolumeError> {
        let dev = Arc::new(SrcDevice {
            src: src.clone(),
            start,
            len,
        });
        // superblok se přečte (a ověří se jeho součet) napřed, aby nepodporované vlastnosti dostaly srozumitelnou hlášku
        let sb = fs_erofs::superblock::read(&*dev).map_err(map_err)?;
        let unsupported = sb.feature_incompat & !EROFS_FEATURE_INCOMPAT_SUPPORTED;
        if unsupported != 0 {
            return Err(VolumeError::Unsupported(format!(
                "unsupported EROFS feature(s): {}",
                feature_names(0, unsupported)
            )));
        }
        if sb.extra_devices != 0 {
            return Err(VolumeError::Unsupported(format!(
                "the EROFS image uses {} extra device(s) (blob devices) that are not part of this source; only single-device images can be read",
                sb.extra_devices
            )));
        }
        let fs = Filesystem::open(dev).map_err(map_err)?;
        // kořen se přečte hned, ať vadný obraz selže při otevření, ne až při prvním výpisu
        fs.root_inode().map_err(map_err)?;
        let info = info_text(&fs);
        Ok(ErofsVolume {
            label: fs.superblock().volume_name_str().to_string(),
            root: u64::from(fs.superblock().root_nid),
            info,
            fs,
        })
    }

    pub fn kind(&self) -> FsKind {
        FsKind::Erofs
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn info_text(&self) -> &str {
        &self.info
    }
    pub fn root(&self) -> NodeId {
        NodeId {
            tree: 0,
            ino: self.root,
        }
    }

    fn inode_of(&self, id: NodeId) -> Result<Inode, VolumeError> {
        if id.tree != 0 {
            return Err(VolumeError::NotFound(format!("erofs item #{}", id.ino)));
        }
        self.fs.read_inode(id.ino).map_err(map_err)
    }

    /// Položky adresáře. Nečitelný inode položky se nevynechává: položka se ukáže bez údajů (typ podle záznamu v adresáři)
    /// a chyba se ohlásí v `warnings`. Nečitelný celý adresář je chyba adresáře (volající ji ohlásí a jde dál).
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Listing, VolumeError> {
        use fs_erofs::dir::ftype;
        let inode = self.inode_of(dir)?;
        let entries = self.fs.read_dir(&inode).map_err(map_err)?;
        let sb = self.fs.superblock();
        let mut nodes = Vec::with_capacity(entries.len());
        let mut warnings = Vec::new();
        for e in entries {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let id = NodeId {
                tree: 0,
                ino: e.nid,
            };
            match self.fs.read_inode(e.nid) {
                Ok(i) => {
                    let kind = if i.is_dir() {
                        NodeKind::Dir
                    } else if i.is_regular_file() {
                        NodeKind::File
                    } else if i.is_symlink() {
                        NodeKind::Symlink
                    } else {
                        continue; // zařízení, fifo, socket: ve Windows nemají smysl
                    };
                    nodes.push(Node {
                        name,
                        kind,
                        size: if kind == NodeKind::Dir { 0 } else { i.size },
                        mtime: mtime_of(sb, &i),
                        id,
                    });
                }
                Err(err) => {
                    // typ je i v záznamu adresáře, takže položku ukážeme; čtení pak případně selže a ohlásí se zvlášť.
                    // Záznam bez známého typu (0) nebo jiný typ než soubor / adresář / symlink se vynechá jako zařízení.
                    let kind = match e.file_type {
                        ftype::DIR => NodeKind::Dir,
                        ftype::REG_FILE => NodeKind::File,
                        ftype::SYMLINK => NodeKind::Symlink,
                        ftype::UNKNOWN => {
                            warnings.push(format!("'{name}': inode unreadable and the entry has no file type, the item is not shown: {}", map_err(err)));
                            continue;
                        }
                        _ => continue,
                    };
                    warnings.push(format!("'{name}': inode unreadable: {}", map_err(err)));
                    nodes.push(Node {
                        name,
                        kind,
                        size: 0,
                        mtime: None,
                        id,
                    });
                }
            }
        }
        Ok(Listing { nodes, warnings })
    }

    /// Pošle obsah souboru (nebo cíl symlinku) do `out`.
    pub fn read_file_to(&mut self, id: NodeId, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let inode = self.inode_of(id)?;
        if inode.is_dir() {
            return Err(VolumeError::Format("a directory has no contents to read".into()));
        }
        if inode.is_symlink() {
            let target = self.fs.read_symlink_target(&inode).map_err(map_err)?;
            out.write_all(&target)?;
            return Ok(target.len() as u64);
        }
        if !inode.is_regular_file() {
            return Err(VolumeError::Unsupported("special file (device, FIFO, socket)".into()));
        }
        let mut buf = vec![0u8; READ_CHUNK.min(inode.size.max(1) as usize)];
        let mut total = 0u64;
        while total < inode.size {
            let n = (inode.size - total).min(buf.len() as u64) as usize;
            // `read_file` je „všechno nebo chyba“: kratší čtení neexistuje, takže se nemusí hlídat nulový posun
            self.fs.read_file(&inode, total, &mut buf[..n]).map_err(map_err)?;
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> [u8; EROFS_SUPER_BLOCK_SIZE] {
        let mut b = [0u8; EROFS_SUPER_BLOCK_SIZE];
        b[0..4].copy_from_slice(&EROFS_SUPER_MAGIC_V1.to_le_bytes());
        b[0x0C] = 12; // blkszbits
        b[0x24..0x28].copy_from_slice(&256u32.to_le_bytes()); // blocks
        b
    }

    #[test]
    fn superblock_plausibility() {
        assert!(plausible_super(&sample()));
        let mut b = sample();
        b[0] = b'x';
        assert!(!plausible_super(&b), "magic");
        let mut b = sample();
        b[0x0C] = 8;
        assert!(!plausible_super(&b), "block size below 512 B");
        let mut b = sample();
        b[0x0C] = 17;
        assert!(!plausible_super(&b), "block size above 64 KiB");
        let mut b = sample();
        b[0x24..0x28].copy_from_slice(&0u32.to_le_bytes());
        assert!(!plausible_super(&b), "at least one block");
        assert!(!plausible_super(&sample()[..100]));
        // nepodporovaná vlastnost ani nenulové dirblkbits nesmí zabránit rozpoznání (hlásí se až při otevření)
        let mut b = sample();
        b[0x50..0x54].copy_from_slice(&0x180u32.to_le_bytes());
        b[0x5A] = 1;
        assert!(plausible_super(&b));
    }

    #[test]
    fn uuid_formatting() {
        let u = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        assert_eq!(uuid_text(&u), "12345678-9abc-def0-1122-334455667788");
    }

    #[test]
    fn features() {
        assert_eq!(feature_names(0, 0), "");
        assert_eq!(feature_names(0x1, 0x1 | 0x2 | 0x10), "sb_chksum zero_padding compr_cfgs/big_pcluster ztailpacking");
        assert_eq!(feature_names(0, 0x80 | 0x100), "48bit metabox");
    }

    #[test]
    fn errors_are_classified() {
        assert!(map_err(ErofsError::Block(BlockError::Io(io::Error::other("x")))).is_io());
        assert!(matches!(map_err(ErofsError::NotFound), VolumeError::NotFound(_)));
        assert!(matches!(map_err(ErofsError::UnsupportedLayout(7)), VolumeError::Unsupported(_)));
        assert!(matches!(
            map_err(ErofsError::BadSuperblock("feature_incompat metabox (0x100) is not implemented by this reader")),
            VolumeError::Unsupported(_)
        ));
        assert!(matches!(
            map_err(ErofsError::BadSuperblock("feature_incompat carries an unknown bit this reader does not implement")),
            VolumeError::Unsupported(_)
        ));
        assert!(matches!(map_err(ErofsError::BadSuperblock("superblock checksum mismatch")), VolumeError::Format(_)));
        assert!(matches!(map_err(ErofsError::BadInode("x")), VolumeError::Format(_)));
        assert!(matches!(map_err(ErofsError::BadDirent("x")), VolumeError::Format(_)));
    }
}
