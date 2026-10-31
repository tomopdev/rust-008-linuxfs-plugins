//! XFS jen pro čtení nad crate `rust-fs-xfs` (MIT, čistý Rust; v4 i v5, `bigtime`, `reflink`, `rmapbt`, `finobt`, `inobtcount`, sparse inody).
//!
//! Položky se adresují skutečným číslem inode XFS (`NodeId.ino`), takže se nevede žádná tabulka cest a číslo platí
//! pro celý svazek. Hard linky mají stejné číslo. XFS má jen jeden strom, proto je `NodeId.tree` vždy 0.
//!
//! Nečistý žurnál crate přehraje jen v paměti (zdroj se nikdy nemění), takže i svazek po pádu nebo nečistém vypnutí
//! se ukáže tak, jak by vypadal po obnově jádrem. Na v5 se ověřuje CRC32C a identita (UUID, skupina, číslo bloku) každého
//! bloku metadat; data souborů kontrolní součty nemají. Svazek s inody na seznamu „unlinked“ (smazané, ale ještě
//! otevřené soubory po pádu) crate odmítne otevřít, protože jejich uvolnění by byl zápis (viz [`map_err`]).

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use fs_core::BlockRead;
use fs_xfs::inode::{FileType, Format};
use fs_xfs::superblock::{incompat, ro_compat};
use fs_xfs::{Error as XfsError, Filesystem, Superblock};

use crate::blockdev::SrcDevice;
use crate::detect::FsKind;
use crate::source::DiskSource;
use crate::volume::{Listing, Node, NodeId, NodeKind, VolumeError};

/// Magie `XFSB` na začátku svazku (superblok je vždy v prvním sektoru).
const MAGIC: &[u8; 4] = b"XFSB";
/// Kolik bajtů superbloku `probe` čte (struktura je 264 B, 512 B je nejmenší sektor).
const PROBE_LEN: usize = 512;
/// Čtení souboru po kusech této velikosti (každé volání čtečky znovu sestaví seznam extentů souboru, proto ne po malých dávkách).
const READ_CHUNK: usize = 4 * 1024 * 1024;
/// Kus dat, který se čte přímo ze zdroje při čtení po extentech.
const EXTENT_CHUNK: u64 = 4 * 1024 * 1024;

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}

/// Věrohodný superblok XFS? Magie `XFSB` sama nestačí (čtyři bajty může mít i jiný soubor), proto se ověřují i velikost
/// bloku (mocnina dvou shodná s `blocklog`), sektoru a inodu, nenulový počet skupin a bloků a verze 4 nebo 5.
///
/// Záměrně se NEověřuje CRC ani podporované vlastnosti: svazek, který crate nezvládne (neznámá vlastnost, špatný
/// kontrolní součet), se má poznat jako XFS a při otevření ohlásit skutečný důvod, ne tiše zmizet z nabídky.
fn plausible_super(b: &[u8]) -> bool {
    if b.len() < PROBE_LEN || &b[0..4] != MAGIC {
        return false;
    }
    let (blocksize, blocklog, sectlog, inodelog) = (be32(b, 4), b[120], b[121], b[122]);
    (9..=16).contains(&blocklog)
        && blocksize == 1u32 << blocklog
        && (9..=15).contains(&sectlog)
        && sectlog <= blocklog
        && (8..=11).contains(&inodelog)
        && inodelog <= blocklog
        && matches!(be16(b, 100) & 0x000F, 4 | 5) // sb_versionnum: dolní čtyři bity = verze
        && be64(b, 8) != 0 // dblocks
        && be32(b, 84) != 0 // agblocks
        && be32(b, 88) != 0 // agcount
        && be64(b, 56) != 0 // rootino
}

/// Začíná na `start` XFS?
pub fn probe(src: &mut DiskSource, start: u64, len: u64) -> bool {
    // `Filesystem::mount` čte při otevření první 4 KiB, menší svazek XFS být nemůže (mkfs.xfs chce aspoň 300 MB)
    if len < 4096 {
        return false;
    }
    let mut b = [0u8; PROBE_LEN];
    src.read_exact_at(start, &mut b).is_ok() && plausible_super(&b)
}

fn map_err(e: XfsError) -> VolumeError {
    match e {
        // chyba čtení zdroje (fs_core::Error::Io převedený na text): zůstává I/O chybou, pluginy ji hlásí jako chybu čtení
        XfsError::Io(m) => VolumeError::Io(io::Error::other(m)),
        XfsError::NotFound => VolumeError::NotFound("XFS item".into()),
        // vlastnost, kterou crate neumí, a data na realtime zařízení, které nemáme
        e @ (XfsError::UnsupportedFeature(_) | XfsError::RealtimeDeviceAbsent { .. }) => VolumeError::Unsupported(e.to_string()),
        // jen čtení: `DirtyLog` crate vrací, když na čistém žurnálu zůstaly inody na seznamu unlinked
        XfsError::DirtyLog => VolumeError::Unsupported(
            "the XFS volume was not unmounted cleanly: it has deleted but still open files (unlinked inodes) whose release \
             would need a write. Mount it in Linux once, or run xfs_repair, and try again"
                .into(),
        ),
        // crate hlásí i poškozený inod nebo blok adresáře jako „malformed XFS superblock“, což by uživatele mátlo
        XfsError::BadSuperblock(m) => VolumeError::Format(format!("damaged XFS structure: {m}")),
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

/// Názvy zapnutých vlastností: v5 podle masek `incompat` / `ro_compat`, v4 podle `features2`.
fn feature_names(sb: &Superblock) -> String {
    const INCOMPAT: [(u32, &str); 9] = [
        (incompat::FTYPE, "ftype"),
        (incompat::SPINODES, "sparse_inodes"),
        (incompat::META_UUID, "meta_uuid"),
        (incompat::BIGTIME, "bigtime"),
        (incompat::NEEDSREPAIR, "needsrepair"),
        (incompat::NREXT64, "nrext64"),
        (incompat::EXCHRANGE, "exchange_range"),
        (incompat::PARENT, "parent_pointers"),
        (incompat::METADIR, "metadir"),
    ];
    const RO: [(u32, &str); 4] = [
        (ro_compat::FINOBT, "finobt"),
        (ro_compat::RMAPBT, "rmapbt"),
        (ro_compat::REFLINK, "reflink"),
        (ro_compat::INOBTCNT, "inobtcount"),
    ];
    // v4: příznaky ze `sb_features2` (jen když je v `sb_versionnum` bit MOREBITS) a přímo ve verzi
    const V4: [(u32, &str); 4] = [(0x2, "lazy_count"), (0x8, "attr2"), (0x80, "projid32"), (0x200, "ftype")];
    let mut v: Vec<&str> = Vec::new();
    if sb.is_v5() {
        v.push("crc");
        v.extend(INCOMPAT.iter().filter(|(b, _)| sb.features_incompat & b != 0).map(|x| x.1));
        v.extend(RO.iter().filter(|(b, _)| sb.features_ro_compat & b != 0).map(|x| x.1));
    } else if sb.versionnum & 0x8000 != 0 {
        v.extend(V4.iter().filter(|(b, _)| sb.features2 & b != 0).map(|x| x.1));
    }
    if sb.has_case_insensitive_dirs() {
        v.push("ascii-ci");
    }
    v.join(" ")
}

fn info_text(sb: &Superblock, replayed: bool) -> String {
    let bs = u64::from(sb.blocksize);
    let size = sb.dblocks.saturating_mul(bs);
    let free = sb.fdblocks.saturating_mul(bs);
    let mut t = String::new();
    t.push_str("xfs file system (read-only view)\r\n\r\n");
    t.push_str(&format!("Label:        {}\r\n", sb.fname));
    t.push_str(&format!("UUID:         {}\r\n", uuid_text(&sb.uuid)));
    t.push_str(&format!(
        "Version:      {}\r\n",
        if sb.is_v5() { "v5 (metadata checksums)" } else { "v4 (no metadata checksums)" }
    ));
    t.push_str(&format!("Block size:   {} B\r\n", sb.blocksize));
    t.push_str(&format!("Size:         {size} B ({} blocks)\r\n", sb.dblocks));
    // fdblocks je u „líných“ čítačů jen přibližné (jádro ho při čistém odpojení přepíše přesným součtem)
    t.push_str(&format!("Free:         {free} B ({} blocks, as recorded in the superblock)\r\n", sb.fdblocks));
    t.push_str(&format!(
        "Allocation groups: {} x {} blocks\r\n",
        sb.agcount, sb.agblocks
    ));
    t.push_str(&format!(
        "Inodes:       {} ({} free), inode size {} B\r\n",
        sb.icount, sb.ifree, sb.inodesize
    ));
    t.push_str(&format!("Features:     {}\r\n", feature_names(sb)));
    // svazek s žurnálem na samostatném zařízení crate neotevře (nelze zjistit, zda je žurnál čistý), takže tu je vždy vnitřní
    t.push_str(&format!("Log:          internal, {} blocks\r\n", sb.logblocks));
    if sb.rblocks != 0 {
        t.push_str(&format!(
            "\r\nThe volume has a realtime section ({} blocks) on a separate device. Files stored there cannot be read.\r\n",
            sb.rblocks
        ));
    }
    if replayed {
        t.push_str("\r\nThe log has not been replayed (the file system was not cleanly unmounted). Nothing is written to the\r\n");
        t.push_str("source: the plugin replays the log in memory only, so you see the state a mount would produce after recovery.\r\n");
    } else {
        t.push_str("\r\nThe log is clean.\r\n");
    }
    t.push_str("\r\nOn v5 volumes the checksums and identity of all metadata are verified; file data has no checksums.\r\n");
    t.push_str("Special files (devices, FIFOs, sockets) are not shown.\r\n");
    t
}

// ---------------------------------------------------------------------------------------------------------------------
// svazek
// ---------------------------------------------------------------------------------------------------------------------

pub struct XfsVolume {
    fs: Filesystem,
    /// Týž zdroj, který čte crate; data souborů se čtou přímo z něj (viz [`XfsVolume::read_extents`]).
    dev: Arc<SrcDevice>,
    label: String,
    info: String,
    root: u64,
}

impl XfsVolume {
    pub fn open(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<Self, VolumeError> {
        let dev = Arc::new(SrcDevice {
            src: src.clone(),
            start,
            len,
        });
        let fs = Filesystem::mount(dev.clone()).map_err(map_err)?;
        // kořen se přečte hned, ať vadný svazek selže při otevření, ne až při prvním výpisu
        fs.root_inode().map_err(map_err)?;
        let sb = fs.superblock();
        Ok(XfsVolume {
            dev,
            label: sb.fname.clone(),
            info: info_text(sb, fs.was_replayed()),
            root: sb.rootino,
            fs,
        })
    }

    pub fn kind(&self) -> FsKind {
        FsKind::Xfs
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

    fn read_inode(&self, id: NodeId) -> Result<(fs_xfs::inode::Inode, Vec<u8>), VolumeError> {
        if id.tree != 0 {
            return Err(VolumeError::NotFound(format!("xfs item #{}", id.ino)));
        }
        self.fs.read_inode_raw(id.ino).map_err(map_err)
    }

    /// Položky adresáře. Nečitelný inode položky se nevynechává: položka se ukáže bez údajů (typ podle záznamu
    /// v adresáři) a chyba se ohlásí v `warnings`. Nečitelný celý adresář je chyba adresáře (volající ji ohlásí a jde dál).
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Listing, VolumeError> {
        let (inode, raw) = self.read_inode(dir)?;
        let entries = self.fs.read_dir(&inode, &raw).map_err(map_err)?;
        let mut nodes = Vec::with_capacity(entries.len());
        let mut warnings = Vec::new();
        for e in entries {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let id = NodeId {
                tree: 0,
                ino: e.ino,
            };
            match self.fs.read_inode(e.ino) {
                Ok(i) => {
                    let (kind, size) = match i.file_type() {
                        Some(FileType::Directory) => (NodeKind::Dir, 0),
                        Some(FileType::Regular) => (NodeKind::File, i.size),
                        Some(FileType::Symlink) => (NodeKind::Symlink, i.size),
                        _ => continue, // zařízení, fifo, socket: ve Windows nemají smysl
                    };
                    nodes.push(Node {
                        name,
                        kind,
                        size,
                        mtime: Some(i.mtime.sec),
                        id,
                    });
                }
                Err(err) => {
                    // typ bývá i v záznamu adresáře (vlastnost ftype), takže položku ukážeme; čtení pak případně selže
                    // a ohlásí se zvlášť. Bez typu nevíme, čím položka je, tak ji aspoň ohlásíme.
                    let kind = match e.ftype {
                        Some(FileType::Directory) => NodeKind::Dir,
                        Some(FileType::Regular) => NodeKind::File,
                        Some(FileType::Symlink) => NodeKind::Symlink,
                        Some(_) => continue,
                        None => {
                            warnings.push(format!("'{name}': inode unreadable and the entry has no file type, the item is not shown: {}", map_err(err)));
                            continue;
                        }
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

    /// Čtení souboru po extentech přímo ze zdroje. `Filesystem::read_at` čte po jednotlivých blocích přes svou
    /// blokovou cache a hledá extenty při každém volání; na velkých souborech to dávalo kolem 13 MB/s. Tady se seznam
    /// extentů sestaví jednou, díry a nezapsané (prealokované) extenty se zapíšou jako nuly a zbytek se čte
    /// po velkých kusech. Metadata (inody, adresáře, strom extentů) dál čte crate včetně kontroly CRC.
    fn read_extents(&self, inode: &fs_xfs::inode::Inode, raw: &[u8], out: &mut dyn Write) -> Result<u64, VolumeError> {
        let extents = self.fs.data_extents(inode, raw).map_err(map_err)?;
        let bs = u64::from(self.fs.superblock().blocksize);
        let zeros = vec![0u8; EXTENT_CHUNK as usize];
        let mut buf = Vec::new();
        let mut pos = 0u64; // kolik bajtů souboru je už odesláno
        let size = inode.size;
        let zero_to = |out: &mut dyn Write, from: &mut u64, to: u64| -> io::Result<()> {
            while *from < to {
                let n = (to - *from).min(EXTENT_CHUNK) as usize;
                out.write_all(&zeros[..n])?;
                *from += n as u64;
            }
            Ok(())
        };
        for e in &extents {
            let start = e.startoff.saturating_mul(bs);
            if start >= size {
                break;
            }
            if start < pos {
                return Err(VolumeError::Format("XFS extents of the file overlap".into()));
            }
            zero_to(out, &mut pos, start)?; // díra před extentem
            let end = e.startoff.saturating_add(e.blockcount).saturating_mul(bs).min(size);
            if e.unwritten {
                zero_to(out, &mut pos, end)?;
                continue;
            }
            let phys = self.fs.superblock().fsblock_offset(e.startblock);
            while pos < end {
                let n = (end - pos).min(EXTENT_CHUNK) as usize;
                buf.resize(n, 0);
                self.dev
                    .read_at(phys + (pos - start), &mut buf)
                    .map_err(|err| VolumeError::Io(io::Error::other(err.to_string())))?;
                out.write_all(&buf)?;
                pos += n as u64;
            }
        }
        zero_to(out, &mut pos, size)?; // díra na konci souboru
        Ok(pos)
    }

    /// Pošle obsah souboru (nebo cíl symlinku) do `out`.
    pub fn read_file_to(&mut self, id: NodeId, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let (inode, raw) = self.read_inode(id)?;
        match inode.file_type() {
            Some(FileType::Directory) => Err(VolumeError::Format("a directory has no contents to read".into())),
            Some(FileType::Symlink) => {
                let target = self.fs.read_link(&inode, &raw).map_err(map_err)?;
                out.write_all(&target)?;
                Ok(target.len() as u64)
            }
            Some(FileType::Regular) if matches!(inode.format, Format::Extents | Format::Btree) && !inode.is_realtime() => {
                self.read_extents(&inode, &raw, out)
            }
            Some(FileType::Regular) => {
                let mut buf = vec![0u8; READ_CHUNK.min(inode.size.max(1) as usize)];
                let mut total = 0u64;
                while total < inode.size {
                    let n = self.fs.read_at(&inode, &raw, total, &mut buf).map_err(map_err)?;
                    if n == 0 {
                        return Err(VolumeError::Format(format!(
                            "the file ends at {total} B, but the inode says {} B",
                            inode.size
                        )));
                    }
                    out.write_all(&buf[..n])?;
                    total += n as u64;
                }
                Ok(total)
            }
            _ => Err(VolumeError::Unsupported("special file (device, FIFO, socket)".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> [u8; PROBE_LEN] {
        let mut b = [0u8; PROBE_LEN];
        b[0..4].copy_from_slice(MAGIC);
        b[4..8].copy_from_slice(&4096u32.to_be_bytes());
        b[8..16].copy_from_slice(&81920u64.to_be_bytes());
        b[56..64].copy_from_slice(&128u64.to_be_bytes());
        b[84..88].copy_from_slice(&20480u32.to_be_bytes());
        b[88..92].copy_from_slice(&4u32.to_be_bytes());
        b[100..102].copy_from_slice(&0xB4B5u16.to_be_bytes()); // v5 s obvyklými bity
        b[120] = 12;
        b[121] = 9;
        b[122] = 9;
        b
    }

    #[test]
    fn superblock_plausibility() {
        // inodelog 9 = 512 B inode (výchozí u v5 je 512)
        assert!(plausible_super(&sample()));
        let mut b = sample();
        b[0] = b'x';
        assert!(!plausible_super(&b), "magic");
        let mut b = sample();
        b[4..8].copy_from_slice(&5000u32.to_be_bytes());
        assert!(!plausible_super(&b), "block size must be 2^blocklog");
        let mut b = sample();
        b[101] = 0xB2;
        assert!(!plausible_super(&b), "version 2 is not supported");
        let mut b = sample();
        b[88..92].copy_from_slice(&0u32.to_be_bytes());
        assert!(!plausible_super(&b), "at least one allocation group");
        let mut b = sample();
        b[121] = 13;
        assert!(!plausible_super(&b), "sector larger than the block");
        let mut b = sample();
        b[122] = 13;
        assert!(!plausible_super(&b), "implausible inode size");
        assert!(!plausible_super(&sample()[..100]));
    }

    #[test]
    fn uuid_formatting() {
        let u = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        assert_eq!(uuid_text(&u), "12345678-9abc-def0-1122-334455667788");
    }

    #[test]
    fn errors_are_classified() {
        assert!(map_err(XfsError::Io("x".into())).is_io());
        assert!(matches!(map_err(XfsError::UnsupportedFeature("x".into())), VolumeError::Unsupported(_)));
        assert!(matches!(map_err(XfsError::DirtyLog), VolumeError::Unsupported(_)));
        assert!(matches!(map_err(XfsError::NotFound), VolumeError::NotFound(_)));
        assert!(matches!(map_err(XfsError::ChecksumMismatch { what: "AGF", block: 1 }), VolumeError::Format(_)));
    }
}
