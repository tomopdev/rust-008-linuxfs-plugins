//! Hledání svazků Linuxu uvnitř zdroje: holý svazek na začátku (obraz oddílu), GPT, MBR (včetně logických oddílů v rozšířeném
//! oddílu) a textové soubory `.linuxfsdev`, které ukazují na surová zařízení (`\\.\PhysicalDrive2` a podobně).
//!
//! Hledá se btrfs, ext2/3/4 a squashfs; další souborové systémy přibudou jako další varianty [`FsKind`].

use std::io;
use std::path::{Path, PathBuf};

use crate::btrfs::SubvolSelect;
use crate::source::DiskSource;

/// Který souborový systém je v oddílu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Btrfs,
    Ext2,
    Ext3,
    Ext4,
    Squashfs,
}

impl FsKind {
    pub fn name(self) -> &'static str {
        match self {
            FsKind::Btrfs => "btrfs",
            FsKind::Ext2 => "ext2",
            FsKind::Ext3 => "ext3",
            FsKind::Ext4 => "ext4",
            FsKind::Squashfs => "squashfs",
        }
    }
}

/// Jeden nalezený svazek.
#[derive(Debug, Clone)]
pub struct Part {
    /// Název oddílu z tabulky oddílů (může být prázdný).
    pub label: String,
    pub start: u64,
    pub len: u64,
    pub kind: FsKind,
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Posun primárního superbloku btrfs od začátku svazku.
const BTRFS_SUPER_OFFSET: u64 = 64 * 1024;

/// Začíná na `start` btrfs? (superblok na +64 KiB: magie `_BHRfS_M` na +0x40, vlastní adresa `bytenr` na +0x30 se musí
/// shodovat s posunem, takže se nechytají náhodné kopie)
pub fn btrfs_at(src: &mut DiskSource, start: u64, len: u64, label: &str) -> Option<Part> {
    if len < BTRFS_SUPER_OFFSET + 4096 {
        return None;
    }
    let mut h = [0u8; 0x48];
    src.read_exact_at(start.checked_add(BTRFS_SUPER_OFFSET)?, &mut h)
        .ok()?;
    (&h[0x40..0x48] == b"_BHRfS_M" && le64(&h, 0x30) == BTRFS_SUPER_OFFSET).then(|| Part {
        label: label.to_string(),
        start,
        len,
        kind: FsKind::Btrfs,
    })
}

/// Souborový systém, který začíná na `start`.
pub fn fs_at(src: &mut DiskSource, start: u64, len: u64, label: &str) -> Option<Part> {
    if let Some(p) = btrfs_at(src, start, len, label) {
        return Some(p);
    }
    if let Some(kind) = crate::ext::probe(src, start, len) {
        return Some(Part {
            label: label.to_string(),
            start,
            len,
            kind,
        });
    }
    crate::squashfs::probe(src, start, len).then(|| Part {
        label: label.to_string(),
        start,
        len,
        kind: FsKind::Squashfs,
    })
}

/// Všechny svazky zdroje: holý svazek na začátku, jinak GPT, jinak MBR.
pub fn find_volumes(src: &mut DiskSource) -> Vec<Part> {
    let total = src.len();
    if let Some(p) = fs_at(src, 0, total, "") {
        return vec![p];
    }
    let parts = gpt(src);
    if !parts.is_empty() {
        return parts;
    }
    mbr(src)
}

fn gpt(src: &mut DiskSource) -> Vec<Part> {
    for sector in [512u64, 4096] {
        let mut h = [0u8; 92];
        if src.read_exact_at(sector, &mut h).is_err() || &h[0..8] != b"EFI PART" {
            continue;
        }
        let entries_lba = le64(&h, 72);
        let count = le32(&h, 80).min(1024) as usize;
        let size = le32(&h, 84) as usize;
        if !(128..=1024).contains(&size) || count == 0 {
            continue;
        }
        let mut table = vec![0u8; count * size];
        if src
            .read_exact_at(entries_lba.saturating_mul(sector), &mut table)
            .is_err()
        {
            continue;
        }
        let mut out = Vec::new();
        for i in 0..count {
            let e = &table[i * size..(i + 1) * size];
            if e[..16].iter().all(|&b| b == 0) {
                continue;
            }
            let (first, last) = (le64(e, 32), le64(e, 40));
            if last < first {
                continue;
            }
            let start = first.saturating_mul(sector);
            let len = (last - first + 1).saturating_mul(sector);
            let units: Vec<u16> = e[56..128.min(size)]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            let name = String::from_utf16_lossy(&units);
            if let Some(p) = fs_at(src, start, len, &name) {
                out.push(p);
            }
        }
        return out;
    }
    Vec::new()
}

/// Typy rozšířeného oddílu MBR (CHS, LBA, Linux).
fn is_extended(t: u8) -> bool {
    matches!(t, 0x05 | 0x0F | 0x85)
}

fn mbr(src: &mut DiskSource) -> Vec<Part> {
    let mut b = [0u8; 512];
    if src.read_exact_at(0, &mut b).is_err() || b[510] != 0x55 || b[511] != 0xAA {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..4 {
        let e = &b[446 + 16 * i..446 + 16 * (i + 1)];
        let t = e[4];
        if t == 0x00 || t == 0xEE {
            continue; // prázdný, ochranný oddíl GPT
        }
        let start = le32(e, 8) as u64 * 512;
        let len = le32(e, 12) as u64 * 512;
        if is_extended(t) {
            logical_partitions(src, start, len, &mut out);
        } else if let Some(p) = fs_at(src, start, len, "") {
            out.push(p);
        }
    }
    out
}

/// Řetěz EBR rozšířeného oddílu: první záznam je logický oddíl (posun vůči tomuto EBR), druhý ukazuje na další EBR
/// (posun vůči začátku rozšířeného oddílu).
fn logical_partitions(src: &mut DiskSource, ext_start: u64, ext_len: u64, out: &mut Vec<Part>) {
    let mut ebr = ext_start;
    for _ in 0..128 {
        let mut b = [0u8; 512];
        if src.read_exact_at(ebr, &mut b).is_err() || b[510] != 0x55 || b[511] != 0xAA {
            return;
        }
        let first = &b[446..462];
        if first[4] != 0 && !is_extended(first[4]) {
            let start = ebr + le32(first, 8) as u64 * 512;
            let len = le32(first, 12) as u64 * 512;
            if let Some(p) = fs_at(src, start, len, "") {
                out.push(p);
            }
        }
        let next = &b[462..478];
        if !is_extended(next[4]) {
            return;
        }
        let rel = le32(next, 8) as u64 * 512;
        if rel == 0 || rel >= ext_len {
            return;
        }
        ebr = ext_start + rel;
    }
}

// ---------------------------------------------------------------------------
// .linuxfsdev: odkaz na zařízení nebo obraz
// ---------------------------------------------------------------------------

/// Obsah souboru `.linuxfsdev`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DevSpec {
    pub device: PathBuf,
    pub offset: Option<u64>,
    pub length: Option<u64>,
    /// Pořadové číslo (od 1) mezi svazky nalezenými v tabulce oddílů.
    pub partition: Option<usize>,
    pub subvol: SubvolSelect,
}

/// Odkaz na zařízení (`.linuxfsdev`)?
pub fn is_devlink(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("linuxfsdev"))
}

fn parse_num(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse::<u64>(),
    };
    r.map_err(|_| format!("invalid number: {s}"))
}

/// Dekóduje text: UTF-8 (s BOM i bez) nebo UTF-16 LE/BE s BOM.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let u: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let u: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Řádky `klíč = hodnota` (`#` / `;` komentáře): `device`, `offset`, `length`, `partition`, `subvol`, `subvolid`.
/// `subvol` je cesta podsvazku od top-level svazku (`@`, `@/nested`) nebo `default` (výchozí podsvazek) či `top`.
pub fn parse_devlink(text: &str) -> Result<DevSpec, String> {
    let mut spec = DevSpec::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("expected key=value: {line}"));
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "device" | "path" => spec.device = PathBuf::from(v.trim_matches('"')),
            "offset" => spec.offset = Some(parse_num(v)?),
            "length" => spec.length = Some(parse_num(v)?),
            "partition" => {
                let n = parse_num(v)? as usize;
                if n == 0 {
                    return Err("partition is 1-based".into());
                }
                spec.partition = Some(n);
            }
            "subvol" => {
                let v = v.trim_matches('"');
                spec.subvol = match v.to_ascii_lowercase().as_str() {
                    "" | "top" | "toplevel" | "top-level" | "/" => SubvolSelect::TopLevel,
                    "default" => SubvolSelect::Default,
                    _ => SubvolSelect::Path(v.trim_matches('/').to_string()),
                };
            }
            "subvolid" => {
                let id = parse_num(v)?;
                spec.subvol = if id == 5 {
                    SubvolSelect::TopLevel
                } else {
                    SubvolSelect::Id(id)
                };
            }
            other => return Err(format!("unknown key: {other}")),
        }
    }
    if spec.device.as_os_str().is_empty() {
        return Err("missing device=".into());
    }
    Ok(spec)
}

/// Proč se zdroj nepodařilo otevřít.
#[derive(Debug)]
pub enum LocateError {
    Io(io::Error),
    Config(String),
}

/// Otevře obraz nebo surové zařízení.
pub fn open_source(path: &Path) -> Result<DiskSource, LocateError> {
    let mut src = DiskSource::open(path).map_err(LocateError::Io)?;
    src.set_name(path.file_name().map(|n| n.to_string_lossy().into_owned()));
    Ok(src)
}

/// Otevře `path` (obraz, zařízení nebo odkaz `.linuxfsdev`) a najde v něm svazky; vrací i zvolený podsvazek.
pub fn locate(path: &Path) -> Result<(DiskSource, Vec<Part>, SubvolSelect), LocateError> {
    if !is_devlink(path) {
        let mut src = open_source(path)?;
        let parts = find_volumes(&mut src);
        return Ok((src, parts, SubvolSelect::TopLevel));
    }
    let bytes = std::fs::read(path).map_err(LocateError::Io)?;
    let spec = parse_devlink(&decode_text(&bytes)).map_err(LocateError::Config)?;
    let mut src = open_source(&spec.device)?;
    let mut parts = match spec.offset {
        Some(off) => {
            let len = spec.length.unwrap_or_else(|| src.len().saturating_sub(off));
            fs_at(&mut src, off, len, "").into_iter().collect()
        }
        None => find_volumes(&mut src),
    };
    if let Some(n) = spec.partition {
        parts = parts.get(n - 1).cloned().into_iter().collect();
    }
    Ok((src, parts, spec.subvol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devlink_parsing() {
        let s = parse_devlink(
            "# comment\ndevice = \\\\.\\PhysicalDrive2\npartition=2\noffset = 0x10\nsubvol=@home\n",
        )
        .unwrap();
        assert_eq!(s.device, PathBuf::from("\\\\.\\PhysicalDrive2"));
        assert_eq!(s.partition, Some(2));
        assert_eq!(s.offset, Some(16));
        assert_eq!(s.subvol, SubvolSelect::Path("@home".into()));
        assert_eq!(
            parse_devlink("device=x\nsubvol=default").unwrap().subvol,
            SubvolSelect::Default
        );
        assert_eq!(
            parse_devlink("device=x\nsubvol=/").unwrap().subvol,
            SubvolSelect::TopLevel
        );
        assert_eq!(
            parse_devlink("device=x\nsubvolid=257").unwrap().subvol,
            SubvolSelect::Id(257)
        );
        assert_eq!(
            parse_devlink("device=x\nsubvolid=5").unwrap().subvol,
            SubvolSelect::TopLevel
        );
        assert!(parse_devlink("offset=1").is_err());
        assert!(parse_devlink("device=x\nfoo=1").is_err());
        assert!(parse_devlink("device=x\npartition=0").is_err());
        assert!(is_devlink(Path::new("a.linuxfsdev")) && is_devlink(Path::new("a.LINUXFSDEV")));
        assert!(!is_devlink(Path::new("a.img")));
    }

    #[test]
    fn utf16_bom() {
        let mut b = vec![0xFF, 0xFE];
        for u in "device=x".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_text(&b), "device=x");
    }
}
