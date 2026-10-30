//! SquashFS: obrazy v `tests/fixtures` vyrobil `mksquashfs` ze zdrojového adresáře (viz `tools/make-squashfs-fixtures.sh`),
//! `squashfs.manifest` obsahuje typ, velikost, mtime a sha256 podle zdroje. Čtečka se musí shodovat na každém bajtu
//! u všech komprese (gzip, xz, zstd, lz4, lzo) a velikostí bloku.

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
    let bytes = std::fs::read(fixture("squashfs.manifest")).unwrap();
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

fn compare(name: &str, compression: &str) {
    let mut vol = open(&unpack(name));
    assert_eq!(vol.kind(), FsKind::Squashfs);
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
    assert!(info.contains(&format!("Compression:  {compression}")), "{info}");
}

#[test]
fn gzip_matches_the_source() {
    compare("squashfs-gzip", "gzip");
}

#[test]
fn xz_matches_the_source() {
    compare("squashfs-xz", "xz");
}

#[test]
fn zstd_matches_the_source() {
    compare("squashfs-zstd", "zstd");
}

#[test]
fn lz4_matches_the_source() {
    compare("squashfs-lz4", "lz4");
}

#[test]
fn lzo_without_fragments_and_exports_matches_the_source() {
    compare("squashfs-lzo", "lzo");
}

#[test]
fn info_file_symlinks_and_hidden_specials() {
    let mut vol = open(&unpack("squashfs-gzip"));
    let root = vol.root();
    let list = vol.list_directory(root).unwrap();
    let info = list.iter().find(|n| n.name == volume::info_name_for(FsKind::Squashfs)).unwrap();
    assert_eq!(info.name, "!SQUASHFS-INFO.txt");
    let mut buf = Vec::new();
    vol.read_file_to(info.id, &mut buf).unwrap();
    assert_eq!(buf.len() as u64, info.size);
    let text = String::from_utf8(buf).unwrap();
    assert!(text.contains("Block size:   131072 B") && text.contains("exportable"), "{text}");
    // FIFO a zařízení se nezobrazují
    assert!(!list.iter().any(|n| n.name == "fifo" || n.name == "chardev"));
    let link = list.iter().find(|n| n.name == "link-rel").unwrap();
    assert_eq!(link.kind, NodeKind::Symlink);
    let mut t = Vec::new();
    vol.read_file_to(link.id, &mut t).unwrap();
    assert_eq!(t, b"hello.txt");
    assert_eq!(link.size, 9);
    // adresář nejde číst jako soubor
    let d = list.iter().find(|n| n.name == "deep").unwrap();
    assert!(vol.read_file_to(d.id, &mut Vec::new()).is_err());
    // prázdný adresář a velký adresář přes víc metadatových bloků
    let e = list.iter().find(|n| n.name == "empty-dir").unwrap();
    assert!(vol.list_directory(e.id).unwrap().is_empty());
    let many = list.iter().find(|n| n.name == "many").unwrap();
    assert_eq!(vol.list_directory(many.id).unwrap().len(), 1500);
    // squashfs nemá jmenovku
    assert_eq!(vol.name(), "");
}

#[test]
fn partition_table_with_squashfs_ext4_and_btrfs() {
    // MBR se třemi oddíly s nenulovým posunem: čtečka musí adresovat vždy uvnitř oddílu
    let images = [std::fs::read(unpack("squashfs-zstd")).unwrap(), std::fs::read(unpack("ext4")).unwrap(), std::fs::read(unpack("btrfs-small")).unwrap()];
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
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-mbr-sq.img", std::process::id()));
    std::fs::write(&path, &disk).unwrap();
    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![FsKind::Squashfs, FsKind::Ext4, FsKind::Btrfs]);
    let src = Arc::new(Mutex::new(src));
    let set = volume::open_all_report(&src, &parts, &SubvolSelect::TopLevel).unwrap();
    assert_eq!(set.volumes.len(), 3);
    assert!(set.failed.is_empty(), "{:?}", set.failed);
    let mut sq = set.volumes.into_iter().next().unwrap().vol;
    assert_eq!(walk(&mut sq).len(), manifest().len());
    let _ = std::fs::remove_file(path);
}

/// Obraz, ve kterém něco není SquashFS, ale začíná `hsqs`, se nepozná; zkrácený obraz se pozná a hlásí chybu při otevření.
#[test]
fn truncated_image_is_detected_and_fails_to_open_with_an_error() {
    let mut bytes = std::fs::read(unpack("squashfs-gzip")).unwrap();
    bytes.truncate(bytes.len() / 2);
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-sq-trunc.img", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::Squashfs);
    let src = Arc::new(Mutex::new(src));
    let err = volume::open_part(&src, &parts[0], &SubvolSelect::TopLevel).err().expect("a truncated image must not open");
    assert!(!err.to_string().is_empty());
    let _ = std::fs::remove_file(path);
}

/// Poškozený obraz: čtení pokračuje a poškození se hlásí (nic se nevynechá potichu).
#[test]
fn damaged_image_keeps_listing_and_reports_problems() {
    let mut vol = open(&unpack("squashfs-damaged"));
    let mut files = 0usize;
    let mut warnings: Vec<String> = Vec::new();
    let mut failed_dirs: Vec<String> = Vec::new();
    let mut failed_reads: Vec<String> = Vec::new();
    let mut stack = vec![(vol.root(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let l = match vol.list_directory_ex(dir) {
            Ok(l) => l,
            Err(e) => {
                failed_dirs.push(format!("{prefix}: {e}"));
                continue;
            }
        };
        warnings.extend(l.warnings.iter().map(|w| format!("{prefix}: {w}")));
        for n in l.nodes {
            let path = format!("{prefix}/{}", n.name);
            match n.kind {
                NodeKind::Dir => stack.push((n.id, path)),
                _ => {
                    files += 1;
                    if let Err(e) = vol.read_file_to(n.id, &mut io::sink()) {
                        failed_reads.push(format!("{path}: {e}"));
                    }
                }
            }
        }
    }
    // první metadatový blok tabulky adresářů nesl listingy `deep` (30 vnořených adresářů) a začátek `many`: ty se ohlásí
    // jako nečitelné adresáře, zbytek svazku (200 malých souborů, jména, soubory v kořeni) se přečte
    assert!(files > 200, "only {files} files were listed");
    let all = format!("{warnings:?} {failed_dirs:?} {failed_reads:?}");
    assert!(
        failed_dirs.iter().any(|d| d.starts_with("/deep:")) && failed_dirs.iter().any(|d| d.starts_with("/many:")),
        "damaged directories were not reported: {all}"
    );
    assert_eq!(failed_dirs.len(), 2, "only the damaged directories may fail: {all}");
    assert!(failed_reads.iter().any(|r| r.starts_with("/0-compressible.txt")), "damaged file data was not reported: {all}");
    // nekomprimované bloky jiných souborů jsou čitelné: selhat smí jen poškozený soubor 
    assert!(
        failed_reads.iter().all(|r| r.starts_with("/0-compressible.txt")),
        "files in intact blocks must stay readable: {} failures, first: {:?}",
        failed_reads.len(),
        failed_reads.first()
    );
}
