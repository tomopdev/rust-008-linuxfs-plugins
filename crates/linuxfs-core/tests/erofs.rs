//! EROFS: obrazy v `tests/fixtures` vyrobil `mkfs.erofs` ze zdrojového adresáře (viz `tools/make-erofs-fixtures.sh`) a jako root je
//! ověřilo i jádro Linuxu; `erofs.manifest` obsahuje typ, velikost, mtime a sha256 podle zdroje. Čtečka se musí shodovat na každém bajtu
//! u všech variant (bez komprese, LZ4, LZ4HC s fragmenty, LZMA, DEFLATE, ZSTD, chunkové soubory; bloky 1 KiB a 4 KiB; compact i extended inody).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use linuxfs_core::btrfs::SubvolSelect;
use linuxfs_core::detect;
use linuxfs_core::detect::FsKind;
use linuxfs_core::volume::{self, LinuxVolume, NodeKind};
use sha2::{Digest, Sha256};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Rozbalí `*.img.gz` do dočasného souboru (jednou na proces a obraz).
fn unpack(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("linuxfs-test-{}-{name}.img", std::process::id()));
    // Testy běží paralelně: rozbalování se serializuje, aby jiný test nečetl napůl zapsaný obraz.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.exists() {
        let gz = std::fs::File::open(fixture(&format!("{name}.img.gz"))).unwrap();
        let mut dec = flate2::read::GzDecoder::new(gz);
        let mut f = std::fs::File::create(&out).unwrap();
        io::copy(&mut dec, &mut f).unwrap();
    }
    out
}

struct Sink(Sha256, u64);
impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.update(b);
        self.1 += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: char,
    size: u64,
    mtime: i64,
    sha: String,
}

fn open(path: &Path) -> LinuxVolume {
    let (src, parts, _) = detect::locate(path).unwrap();
    assert_eq!(parts.len(), 1);
    let src = Arc::new(Mutex::new(src));
    let mut v = volume::open_all(&src, &parts, &SubvolSelect::TopLevel).unwrap();
    v.remove(0).vol
}

/// Projde celý strom a vrátí cesta -> položka; obsah souborů hashuje.
fn walk(vol: &mut LinuxVolume) -> BTreeMap<String, Entry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(vol.root(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for n in vol.list_directory(dir).unwrap() {
            if n.id == volume::INFO_ID {
                continue;
            }
            let path = if prefix.is_empty() { n.name.clone() } else { format!("{prefix}/{}", n.name) };
            let (kind, sha) = match n.kind {
                NodeKind::Dir => {
                    stack.push((n.id, path.clone()));
                    ('d', "-".to_string())
                }
                k => {
                    let mut s = Sink(Sha256::new(), 0);
                    let got = vol.read_file_to(n.id, &mut s).unwrap();
                    assert_eq!(got, n.size, "{path}");
                    assert_eq!(s.1, n.size, "{path}: written bytes");
                    (if k == NodeKind::Symlink { 'l' } else { 'f' }, format!("{:x}", s.0.finalize()))
                }
            };
            out.insert(path, Entry { kind, size: n.size, mtime: n.mtime.unwrap(), sha });
        }
    }
    out
}

fn manifest() -> BTreeMap<String, Entry> {
    let bytes = std::fs::read(fixture("erofs.manifest")).unwrap();
    let mut m = BTreeMap::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let mut p = line.splitn(5, '\t');
        let kind = p.next().unwrap().chars().next().unwrap();
        let size = p.next().unwrap().parse().unwrap();
        let mtime = p.next().unwrap().parse().unwrap();
        let sha = p.next().unwrap().to_string();
        m.insert(p.next().unwrap().to_string(), Entry { kind, size, mtime, sha });
    }
    m
}


fn compare(name: &str, compressors: &str) {
    let mut vol = open(&unpack(name));
    assert_eq!(vol.kind(), FsKind::Erofs);
    let got = walk(&mut vol);
    let want = manifest();
    for (p, w) in &want {
        match got.get(p) {
            None => panic!("{name}: missing in the plugin view: {p:?}"),
            Some(g) => assert_eq!(g, w, "{name}: {p:?}"),
        }
    }
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    assert!(extra.is_empty(), "{name}: extra entries: {extra:?}");
    let info = vol.info_text_for_tests();
    assert!(info.contains(&format!("Compressors:  {compressors}")), "{name}: {info}");
}

const NONE: &str = "none declared (uncompressed image, or legacy LZ4)";

#[test]
fn plain_matches_the_source() {
    compare("erofs-plain", NONE);
}

#[test]
fn plain_with_1k_blocks_matches_the_source() {
    compare("erofs-plain-1k", NONE);
}

/// Extended inody mají vlastní čas změny; compact inody (ostatní obrazy) ho berou z času vytvoření obrazu: obojí musí dát čas ze zdroje.
#[test]
fn extended_inodes_match_the_source() {
    compare("erofs-extended", NONE);
}

#[test]
fn lz4_matches_the_source() {
    compare("erofs-lz4", NONE); // jednoblokové pclustery LZ4 nepotřebují COMPR_CFGS
}

#[test]
fn lz4hc_with_fragments_and_dedupe_matches_the_source() {
    compare("erofs-lz4hc", "lz4");
}

#[test]
fn lzma_matches_the_source() {
    compare("erofs-lzma", "lzma");
}

#[test]
fn deflate_matches_the_source() {
    compare("erofs-deflate", "deflate");
}

#[test]
fn zstd_matches_the_source() {
    compare("erofs-zstd", "zstd");
}

#[test]
fn chunked_files_with_holes_match_the_source() {
    compare("erofs-chunked", NONE);
}

#[test]
fn info_file_symlinks_hard_links_and_hidden_specials() {
    let mut vol = open(&unpack("erofs-plain"));
    let root = vol.root();
    let list = vol.list_directory(root).unwrap();
    let info = list.iter().find(|n| n.name == volume::info_name_for(FsKind::Erofs)).unwrap();
    assert_eq!(info.name, "!EROFS-INFO.txt");
    let mut buf = Vec::new();
    vol.read_file_to(info.id, &mut buf).unwrap();
    assert_eq!(buf.len() as u64, info.size);
    let text = String::from_utf8(buf).unwrap();
    assert!(text.contains("Label:        EROFS-PLAIN") && text.contains("UUID:         12345678-9abc-def0-1122-334455667788"), "{text}");
    assert!(text.contains("Block size:   4096 B") && text.contains("Created:      2026-01-02 03:04:05 UTC"), "{text}");
    // FIFO a zařízení se nezobrazují
    assert!(!list.iter().any(|n| n.name == "fifo" || n.name == "chardev"));
    let link = list.iter().find(|n| n.name == "link-rel").unwrap();
    assert_eq!(link.kind, NodeKind::Symlink);
    let mut t = Vec::new();
    vol.read_file_to(link.id, &mut t).unwrap();
    assert_eq!(t, b"hello.txt");
    assert_eq!(link.size, 9);
    // hard link má stejný inode (NID) jako původní soubor
    let hello = list.iter().find(|n| n.name == "hello.txt").unwrap();
    let hard = list.iter().find(|n| n.name == "hard1.txt").unwrap();
    assert_eq!(hello.id, hard.id);
    // adresář nejde číst jako soubor
    let d = list.iter().find(|n| n.name == "deep").unwrap();
    assert!(vol.read_file_to(d.id, &mut Vec::new()).is_err());
    // prázdný adresář a velký adresář přes víc bloků
    let e = list.iter().find(|n| n.name == "empty-dir").unwrap();
    assert!(vol.list_directory(e.id).unwrap().is_empty());
    let many = list.iter().find(|n| n.name == "many").unwrap();
    assert_eq!(vol.list_directory(many.id).unwrap().len(), 1500);
    assert_eq!(vol.name(), "EROFS-PLAIN");
}

#[test]
fn partition_table_with_erofs_ext4_and_btrfs() {
    // MBR se třemi oddíly s nenulovým posunem: čtečka musí adresovat vždy uvnitř oddílu
    let images = [std::fs::read(unpack("erofs-zstd")).unwrap(), std::fs::read(unpack("ext4")).unwrap(), std::fs::read(unpack("btrfs-small")).unwrap()];
    let mut disk = vec![0u8; 2048 * 512];
    let mut starts = Vec::new();
    for img in &images {
        let start = disk.len();
        starts.push(start);
        disk.extend_from_slice(img);
        disk.resize((disk.len().div_ceil(512) + 2048) * 512, 0);
    }
    for (i, img) in images.iter().enumerate() {
        let o = 446 + 16 * i;
        disk[o + 4] = 0x83;
        disk[o + 8..o + 12].copy_from_slice(&((starts[i] / 512) as u32).to_le_bytes());
        disk[o + 12..o + 16].copy_from_slice(&((img.len().div_ceil(512)) as u32).to_le_bytes());
    }
    disk[510] = 0x55;
    disk[511] = 0xAA;
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-mbr-erofs.img", std::process::id()));
    std::fs::write(&path, &disk).unwrap();
    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![FsKind::Erofs, FsKind::Ext4, FsKind::Btrfs]);
    let src = Arc::new(Mutex::new(src));
    let set = volume::open_all_report(&src, &parts, &SubvolSelect::TopLevel).unwrap();
    assert_eq!(set.volumes.len(), 3);
    assert!(set.failed.is_empty(), "{:?}", set.failed);
    let mut er = set.volumes.into_iter().next().unwrap().vol;
    assert_eq!(walk(&mut er).len(), manifest().len());
    let _ = std::fs::remove_file(path);
}

/// Zkrácený obraz se pozná a hlásí chybu při otevření.
#[test]
fn truncated_image_is_detected_and_fails_to_open_with_an_error() {
    let mut bytes = std::fs::read(unpack("erofs-plain")).unwrap();
    bytes.truncate(bytes.len() / 2);
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-erofs-trunc.img", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::Erofs);
    // zkrácení uprostřed dat nemusí selhat při otevření (kořen je na začátku), ale čtení kořenového výpisu a všech souborů
    // se nesmí tvářit, že je v pořádku: někde se musí objevit chyba
    let src = Arc::new(Mutex::new(src));
    match volume::open_part(&src, &parts[0], &SubvolSelect::TopLevel) {
        Err(e) => assert!(!e.to_string().is_empty()),
        Ok(mut vol) => {
            let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut failures = 0usize;
                let mut stack = vec![vol.root()];
                while let Some(dir) = stack.pop() {
                    match vol.list_directory_ex(dir) {
                        Err(_) => failures += 1,
                        Ok(l) => {
                            failures += l.warnings.len();
                            for n in l.nodes {
                                match n.kind {
                                    NodeKind::Dir => stack.push(n.id),
                                    _ => failures += vol.read_file_to(n.id, &mut io::sink()).is_err() as usize,
                                }
                            }
                        }
                    }
                }
                failures
            }));
            assert!(got.unwrap() > 0, "a truncated image must report problems");
        }
    }
    let _ = std::fs::remove_file(path);
}

/// Kopie obrazu s upraveným superblokem: `edit` dostane bajty superbloku (od +1024).
fn patched(name: &str, tag: &str, edit: impl FnOnce(&mut [u8])) -> PathBuf {
    let mut bytes = std::fs::read(unpack(name)).unwrap();
    edit(&mut bytes[1024..1024 + 128]);
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-erofs-{tag}.img", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    path
}

fn open_error(path: &Path) -> volume::VolumeError {
    let (src, parts, _) = detect::locate(path).unwrap();
    assert_eq!(parts.len(), 1, "the damaged superblock must still be recognised as EROFS");
    assert_eq!(parts[0].kind, FsKind::Erofs);
    let src = Arc::new(Mutex::new(src));
    volume::open_part(&src, &parts[0], &SubvolSelect::TopLevel).err().expect("the image must not open")
}

/// Vlastnosti, které čtečka neumí, a obrazy s dalšími zařízeními se odmítnou s vysvětlením (bez součtu, aby se došlo až k nim).
#[test]
fn unsupported_images_are_refused_with_a_reason() {
    let no_crc = |sb: &mut [u8]| sb[0x08] &= !0x01; // feature_compat: bez sb_chksum
    for (tag, bit, what) in [("48bit", 0x80u32, "48bit"), ("metabox", 0x100, "metabox")] {
        let path = patched("erofs-plain", tag, |sb| {
            no_crc(sb);
            let f = u32::from_le_bytes(sb[0x50..0x54].try_into().unwrap()) | bit;
            sb[0x50..0x54].copy_from_slice(&f.to_le_bytes());
        });
        let e = open_error(&path);
        assert!(matches!(e, volume::VolumeError::Unsupported(_)), "{e}");
        assert!(e.to_string().contains(what), "{e}");
        let _ = std::fs::remove_file(path);
    }
    let path = patched("erofs-plain", "blobdev", |sb| {
        no_crc(sb);
        sb[0x56..0x58].copy_from_slice(&1u16.to_le_bytes()); // extra_devices
    });
    let e = open_error(&path);
    assert!(matches!(e, volume::VolumeError::Unsupported(_)), "{e}");
    assert!(e.to_string().contains("extra device"), "{e}");
    let _ = std::fs::remove_file(path);
}

/// Superblok s kontrolním součtem, který nesedí, se odmítne (když ho obraz nese).
#[test]
fn bad_superblock_checksum_is_refused() {
    let path = patched("erofs-plain", "badcrc", |sb| {
        assert!(sb[0x08] & 0x01 != 0, "mkfs.erofs 1.9 writes the superblock checksum by default");
        sb[0x40] ^= 0xFF; // jméno svazku
    });
    let e = open_error(&path);
    assert!(matches!(e, volume::VolumeError::Format(_)), "{e}");
    assert!(e.to_string().to_lowercase().contains("checksum"), "{e}");
    let _ = std::fs::remove_file(path);
}

#[derive(Default)]
struct Outcome {
    files: usize,
    warnings: Vec<String>,
    failed_dirs: Vec<String>,
    failed_reads: Vec<String>,
}

/// Projde celý svazek tak, jak to dělají pluginy: po chybě pokračuje a všechno, co selhalo, si zapamatuje.
fn walk_collecting(vol: &mut LinuxVolume) -> Outcome {
    let mut o = Outcome::default();
    let mut stack = vec![(vol.root(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let l = match vol.list_directory_ex(dir) {
            Ok(l) => l,
            Err(e) => {
                o.failed_dirs.push(format!("{prefix}: {e}"));
                continue;
            }
        };
        o.warnings.extend(l.warnings.iter().map(|w| format!("{prefix}: {w}")));
        for n in l.nodes {
            let path = format!("{prefix}/{}", n.name);
            match n.kind {
                NodeKind::Dir => stack.push((n.id, path)),
                _ => {
                    o.files += 1;
                    if let Err(e) = vol.read_file_to(n.id, &mut io::sink()) {
                        o.failed_reads.push(format!("{path}: {e}"));
                    }
                }
            }
        }
    }
    o
}

/// Poškozený obraz: čtení pokračuje a poškození se hlásí (nic se nevynechá potichu).
#[test]
fn damaged_image_keeps_listing_and_reports_problems() {
    let mut vol = open(&unpack("erofs-damaged"));
    let o = walk_collecting(&mut vol);
    let all = format!("{:?} {:?} {:?}", o.warnings, o.failed_dirs, o.failed_reads);
    // tři zničené inody: dva souborové se ukážou bez údajů s upozorněním (`/big.bin`, `/names/Case.txt`), adresářový (`level3`)
    // se ukáže s upozorněním a jeho výpis selže; první blok adresáře `/many` je vadný, takže jeho výpis selže také
    assert!(o.warnings.iter().any(|w| w.contains("'big.bin'")), "{all}");
    assert!(o.warnings.iter().any(|w| w.contains("'Case.txt'")), "{all}");
    assert!(o.warnings.iter().any(|w| w.contains("'level3'")), "{all}");
    assert!(o.failed_dirs.iter().any(|d| d.starts_with("/many:")), "{all}");
    assert!(o.failed_dirs.iter().any(|d| d.contains("level3")), "{all}");
    assert_eq!(o.failed_dirs.len(), 2, "only the damaged directories may fail: {all}");
    // čtení souboru s nečitelným inodem selže a ohlásí se, ostatní soubory (200 malých, jména, kořen) se přečtou
    assert!(o.failed_reads.iter().any(|r| r.starts_with("/big.bin:")) && o.failed_reads.iter().any(|r| r.starts_with("/names/Case.txt:")), "{all}");
    assert_eq!(o.failed_reads.len(), 2, "{all}");
    assert!(o.files > 200, "only {} files were listed", o.files);
}

/// Poškozená komprimovaná data jednoho souboru: selže jen jeho čtení, ostatní soubory zůstanou čitelné.
#[test]
fn damaged_compressed_stream_fails_only_that_file() {
    let mut vol = open(&unpack("erofs-damaged-lz4"));
    let o = walk_collecting(&mut vol);
    let all = format!("{:?} {:?} {:?}", o.warnings, o.failed_dirs, o.failed_reads);
    assert!(o.warnings.is_empty() && o.failed_dirs.is_empty(), "{all}");
    assert!(
        o.failed_reads.len() == 1 && o.failed_reads[0].starts_with("/0-compressible.txt:"),
        "only the damaged file may fail: {all}"
    );
    assert!(o.files > 200);
}
