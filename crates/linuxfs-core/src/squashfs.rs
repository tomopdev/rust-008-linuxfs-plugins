//! SquashFS (4.0) jen pro čtení nad crate `rust-fs-squashfs` (MIT, čistý Rust; komprese gzip, xz, lzo, lz4, zstd a starý lzma).
//!
//! Položky se adresují odkazem na inode (`NodeId.ino` = „inode reference“ SquashFS: posun metadatového bloku a posun
//! v něm), takže se nemusí vést žádná tabulka cest a číslo platí pro celý obraz. Hard linky mají stejný odkaz.
//! SquashFS nemá jmenovku ani UUID; informační soubor ukazuje kompresi, velikost bloku a příznaky obrazu.
//! Neexistují tu žádné kontrolní součty dat, takže poškození se pozná jen tam, kde selže dekomprese nebo kontrola struktur.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use fs_core::Error as BlockError;
use fs_squashfs::{Error as SqError, FileType, Filesystem, Inode};

use crate::blockdev::SrcDevice;
use crate::detect::FsKind;
use crate::source::DiskSource;
use crate::util;
use crate::volume::{Listing, Node, NodeId, NodeKind, VolumeError};

/// Magie `hsqs` (little-endian `0x73717368`) na začátku obrazu.
const MAGIC: &[u8; 4] = b"hsqs";
const SUPER_LEN: usize = 96;
/// Čtení souboru po kusech této velikosti (každé volání čtečky sestaví mapu bloků souboru, proto ne po malých dávkách).
const READ_CHUNK: usize = 4 * 1024 * 1024;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Věrohodný superblok SquashFS 4.0? Magie sama nestačí (čtyři bajty `hsqs` může mít i jiný soubor), proto se ověřuje
/// verze, shoda velikosti bloku s její mocninou, rozumná velikost bloku a známá komprese.
fn plausible_super(b: &[u8]) -> bool {
    if b.len() < SUPER_LEN || &b[0..4] != MAGIC {
        return false;
    }
    let (block_size, block_log) = (le32(b, 0x0C), le16(b, 0x16));
    (12..=20).contains(&block_log)
        && block_size == 1u32 << block_log
        && le16(b, 0x1C) == 4 // verze 4.0; starší 1.x–3.x čtečka nezná
        && (1..=6).contains(&le16(b, 0x14))
        && le32(b, 0x04) != 0 // aspoň kořen
}

/// Začíná na `start` SquashFS?
pub fn probe(src: &mut DiskSource, start: u64, len: u64) -> bool {
    if len < SUPER_LEN as u64 {
        return false;
    }
    let mut b = [0u8; SUPER_LEN];
    src.read_exact_at(start, &mut b).is_ok() && plausible_super(&b)
}

// ---------------------------------------------------------------------------------------------------------------------
// zdroj dat pro crate
// ---------------------------------------------------------------------------------------------------------------------

fn map_err(e: SqError) -> VolumeError {
    match e {
        SqError::Block(BlockError::Io(io)) => VolumeError::Io(io),
        SqError::Block(other) => VolumeError::Io(io::Error::other(format!("{other:?}"))),
        SqError::UnsupportedCompression(id) => VolumeError::Unsupported(format!("unsupported SquashFS compressor (id {id})")),
        SqError::NotFound => VolumeError::NotFound("SquashFS item".into()),
        other => VolumeError::Format(other.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// informační soubor
// ---------------------------------------------------------------------------------------------------------------------

fn flag_names(flags: u16) -> String {
    const FLAGS: [(u16, &str); 11] = [
        (0x0001, "uncompressed_inodes"),
        (0x0002, "uncompressed_data"),
        (0x0008, "uncompressed_fragments"),
        (0x0010, "no_fragments"),
        (0x0020, "always_fragments"),
        (0x0040, "duplicates"),
        (0x0080, "exportable"),
        (0x0100, "uncompressed_xattrs"),
        (0x0200, "no_xattrs"),
        (0x0400, "compressor_options"),
        (0x0800, "uncompressed_ids"),
    ];
    FLAGS
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|x| x.1)
        .collect::<Vec<_>>()
        .join(" ")
}

fn info_text(fs: &Filesystem) -> String {
    let sb = &fs.sb;
    let mut t = String::new();
    t.push_str("squashfs file system (read-only view)\r\n\r\n");
    t.push_str(&format!("Version:      {}.{}\r\n", sb.version_major, sb.version_minor));
    t.push_str(&format!("Compression:  {}\r\n", fs.compressor().name()));
    t.push_str(&format!("Block size:   {} B\r\n", sb.block_size));
    t.push_str(&format!("Image size:   {} B (used by the image)\r\n", sb.bytes_used));
    t.push_str(&format!("Inodes:       {}\r\n", sb.inode_count));
    t.push_str(&format!("Fragments:    {}\r\n", sb.fragment_entry_count));
    t.push_str(&format!("Created:      {} UTC\r\n", util::format_utc(sb.modification_time as i64)));
    t.push_str(&format!("Features:     {}\r\n", flag_names(sb.flags)));
    t.push_str("\r\nSquashFS has no label, UUID or data checksums. Special files (devices, FIFOs, sockets) are not shown.\r\n");
    t
}

// ---------------------------------------------------------------------------------------------------------------------
// svazek
// ---------------------------------------------------------------------------------------------------------------------

pub struct SquashVolume {
    fs: Filesystem,
    info: String,
}

impl SquashVolume {
    pub fn open(src: &Arc<Mutex<DiskSource>>, start: u64, len: u64) -> Result<Self, VolumeError> {
        let dev = Arc::new(SrcDevice {
            src: src.clone(),
            start,
            len,
        });
        let fs = Filesystem::open(dev).map_err(map_err)?;
        // kořen se přečte hned, ať vadný obraz selže při otevření, ne až při prvním výpisu
        fs.root_inode().map_err(map_err)?;
        let info = info_text(&fs);
        Ok(SquashVolume { fs, info })
    }

    pub fn kind(&self) -> FsKind {
        FsKind::Squashfs
    }
    /// SquashFS nemá jmenovku.
    pub fn label(&self) -> &str {
        ""
    }
    pub fn info_text(&self) -> &str {
        &self.info
    }
    pub fn root(&self) -> NodeId {
        NodeId {
            tree: 0,
            ino: self.fs.sb.root_inode_ref,
        }
    }

    fn inode_of(&self, id: NodeId) -> Result<Inode, VolumeError> {
        if id.tree != 0 {
            return Err(VolumeError::NotFound(format!("squashfs item #{}", id.ino)));
        }
        self.fs.read_inode(id.ino).map_err(map_err)
    }

    /// Položky adresáře. Nečitelný inode položky se nevynechává: položka se ukáže bez údajů (typ podle záznamu v adresáři)
    /// a chyba se ohlásí v `warnings`. Nečitelný listing celého adresáře je chyba adresáře (volající ji ohlásí a jde dál).
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Listing, VolumeError> {
        let inode = self.inode_of(dir)?;
        let entries = self.fs.read_dir(&inode).map_err(map_err)?;
        let mut nodes = Vec::with_capacity(entries.len());
        let mut warnings = Vec::new();
        for e in entries {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let id = NodeId {
                tree: 0,
                ino: e.inode_ref,
            };
            match self.fs.read_inode(e.inode_ref) {
                Ok(i) => {
                    let (kind, size) = match i.file_type() {
                        FileType::Dir => (NodeKind::Dir, 0),
                        FileType::RegFile => (NodeKind::File, i.file_size),
                        FileType::Symlink => (NodeKind::Symlink, i.symlink_target.len() as u64),
                        _ => continue, // zařízení, fifo, socket: ve Windows nemají smysl
                    };
                    nodes.push(Node {
                        name,
                        kind,
                        size,
                        mtime: Some(i.mtime as i64),
                        id,
                    });
                }
                Err(err) => {
                    // typ je i v záznamu adresáře, takže položku ukážeme; čtení pak případně selže a ohlásí se zvlášť
                    let kind = match FileType::from_type_id(e.type_id) {
                        FileType::Dir => NodeKind::Dir,
                        FileType::RegFile => NodeKind::File,
                        FileType::Symlink => NodeKind::Symlink,
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
        match inode.file_type() {
            FileType::Dir => Err(VolumeError::Format("a directory has no contents to read".into())),
            FileType::Symlink => {
                out.write_all(&inode.symlink_target)?;
                Ok(inode.symlink_target.len() as u64)
            }
            FileType::RegFile => {
                let mut buf = vec![0u8; READ_CHUNK.min(inode.file_size.max(1) as usize)];
                let mut total = 0u64;
                while total < inode.file_size {
                    let n = self.fs.read_file(&inode, total, &mut buf).map_err(map_err)?;
                    if n == 0 {
                        return Err(VolumeError::Format(format!(
                            "the file ends at {total} B, but the inode says {} B",
                            inode.file_size
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

    fn sample() -> [u8; SUPER_LEN] {
        let mut b = [0u8; SUPER_LEN];
        b[0..4].copy_from_slice(MAGIC);
        b[0x04..0x08].copy_from_slice(&5u32.to_le_bytes());
        b[0x0C..0x10].copy_from_slice(&(128 * 1024u32).to_le_bytes());
        b[0x14..0x16].copy_from_slice(&6u16.to_le_bytes());
        b[0x16..0x18].copy_from_slice(&17u16.to_le_bytes());
        b[0x1C..0x1E].copy_from_slice(&4u16.to_le_bytes());
        b
    }

    #[test]
    fn superblock_plausibility() {
        assert!(plausible_super(&sample()));
        let mut b = sample();
        b[0] = b'x';
        assert!(!plausible_super(&b), "magic");
        let mut b = sample();
        b[0x1C] = 3;
        assert!(!plausible_super(&b), "version 3 is not supported");
        let mut b = sample();
        b[0x0C..0x10].copy_from_slice(&(100_000u32).to_le_bytes());
        assert!(!plausible_super(&b), "block size must be 2^block_log");
        let mut b = sample();
        b[0x14] = 9;
        assert!(!plausible_super(&b), "unknown compressor");
        assert!(!plausible_super(&sample()[..50]));
    }

    #[test]
    fn flags() {
        assert_eq!(flag_names(0), "");
        assert_eq!(flag_names(0x0080 | 0x0010), "no_fragments exportable");
    }
}
