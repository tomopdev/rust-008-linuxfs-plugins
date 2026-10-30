//! ext2/3/4: obrazy v `tests/fixtures` vyrobil `mke2fs -d` ze zdrojového adresáře (viz `tools/make-ext-fixtures.sh`),
//! `*.manifest` obsahuje typ, velikost, mtime a sha256 podle zdroje. Čtečka se musí shodovat na každém bajtu.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use linuxfs_core::btrfs::SubvolSelect;
use linuxfs_core::detect::FsKind;
use linuxfs_core::detect;
use linuxfs_core::volume::{self, LinuxVolume, NodeKind};
use sha2::{Digest, Sha256};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Rozbalí `*.img.gz` do dočasného souboru (jednou na proces a obraz).
pub fn unpack(name: &str) -> PathBuf {
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

fn open(path: &Path, select: &SubvolSelect) -> LinuxVolume {
    let (src, parts, _) = detect::locate(path).unwrap();
    assert_eq!(parts.len(), 1);
    let src = Arc::new(Mutex::new(src));
    let mut v = volume::open_all(&src, &parts, select).unwrap();
    v.remove(0).vol
}

/// Projde celý strom a vrátí cesta -> položka; obsah souborů hashuje.
fn walk(vol: &mut LinuxVolume, hash_limit: u64) -> BTreeMap<String, Entry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(vol.root(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for n in vol.list_directory(dir).unwrap() {
            if n.id == volume::INFO_ID {
                continue;
            }
            let path = if prefix.is_empty() {
                n.name.clone()
            } else {
                format!("{prefix}/{}", n.name)
            };
            let (kind, sha) = match n.kind {
                NodeKind::Dir => {
                    stack.push((n.id, path.clone()));
                    ('d', "-".to_string())
                }
                k => {
                    let c = if k == NodeKind::Symlink { 'l' } else { 'f' };
                    if n.size > hash_limit {
                        (c, "-".to_string())
                    } else {
                        let mut s = Sink(Sha256::new(), 0);
                        let got = vol.read_file_to(n.id, &mut s).unwrap();
                        assert_eq!(got, n.size, "{path}");
                        assert_eq!(s.1, n.size, "{path}: written bytes");
                        (c, format!("{:x}", s.0.finalize()))
                    }
                }
            };
            out.insert(
                path,
                Entry {
                    kind,
                    size: n.size,
                    mtime: n.mtime.unwrap(),
                    sha,
                },
            );
        }
    }
    out
}

fn manifest(name: &str) -> BTreeMap<String, Entry> {
    let bytes = std::fs::read(fixture(&format!("{name}.manifest"))).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let mut p = line.splitn(5, '\t');
        let kind = p.next().unwrap().chars().next().unwrap();
        let size = p.next().unwrap().parse().unwrap();
        let mtime = p.next().unwrap().parse().unwrap();
        let sha = p.next().unwrap().to_string();
        let path = p.next().unwrap().to_string();
        m.insert(
            path,
            Entry {
                kind,
                size,
                mtime,
                sha,
            },
        );
    }
    m
}


fn compare(name: &str, kind: FsKind) {
    let img = unpack(name);
    let mut vol = open(&img, &SubvolSelect::TopLevel);
    assert_eq!(vol.kind(), kind);
    assert_eq!(vol.name(), format!("Lbl-{name}"));
    let got = walk(&mut vol, 1 << 30);
    let want = manifest(name);
    for (p, w) in &want {
        match got.get(p) {
            None => panic!("missing in the plugin view: {p:?}"),
            Some(g) => assert_eq!(g, w, "{p:?}"),
        }
    }
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    let extra: Vec<_> = extra.into_iter().filter(|k| *k != "lost+found").collect(); // vytváří mke2fs
    assert!(extra.is_empty(), "extra entries: {extra:?}");
    assert_eq!(got.len(), want.len() + 1, "lost+found + manifest");
}

#[test]
fn ext2_matches_the_source() {
    compare("ext2", FsKind::Ext2);
}

#[test]
fn ext3_matches_the_source() {
    compare("ext3", FsKind::Ext3);
}

#[test]
fn ext4_matches_the_source() {
    compare("ext4", FsKind::Ext4);
}

#[test]
fn info_file_and_hidden_specials() {
    let img = unpack("ext4");
    let mut vol = open(&img, &SubvolSelect::TopLevel);
    let root = vol.root();
    let list = vol.list_directory(root).unwrap();
    let info = list.iter().find(|n| n.name == volume::info_name_for(FsKind::Ext4)).unwrap();
    let mut buf = Vec::new();
    vol.read_file_to(info.id, &mut buf).unwrap();
    assert_eq!(buf.len() as u64, info.size);
    let text = String::from_utf8(buf).unwrap();
    assert!(text.contains("Lbl-ext4") && text.contains("extents") && text.contains("metadata_csum"), "{text}");
    assert!(!list.iter().any(|n| n.name == "fifo"));
    let link = list.iter().find(|n| n.name == "link-rel").unwrap();
    assert_eq!(link.kind, NodeKind::Symlink);
    let mut t = Vec::new();
    vol.read_file_to(link.id, &mut t).unwrap();
    assert_eq!(t, b"hello.txt");
    let d = list.iter().find(|n| n.name == "deep").unwrap();
    assert!(vol.read_file_to(d.id, &mut Vec::new()).is_err());
    // ext3 i ext2 mají vlastní jméno informačního souboru
    let v3 = open(&unpack("ext3"), &SubvolSelect::TopLevel);
    assert_eq!(v3.info_node().name, "!EXT3-INFO.txt");
}

#[test]
fn partition_table_with_ext_and_btrfs() {
    // MBR se dvěma oddíly: ext4 a btrfs z hotových obrazů
    let ext = std::fs::read(unpack("ext4")).unwrap();
    let bt = std::fs::read(unpack("btrfs-small")).unwrap();
    let start1 = 2048u64 * 512;
    let start2 = start1 + ext.len().div_ceil(512) as u64 * 512 + 512 * 2048;
    let mut disk = vec![0u8; (start2 as usize) + bt.len()];
    disk[start1 as usize..start1 as usize + ext.len()].copy_from_slice(&ext);
    disk[start2 as usize..].copy_from_slice(&bt);
    let entry = |i: usize, t: u8, start: u64, len: usize, d: &mut Vec<u8>| {
        let o = 446 + 16 * i;
        d[o + 4] = t;
        d[o + 8..o + 12].copy_from_slice(&((start / 512) as u32).to_le_bytes());
        d[o + 12..o + 16].copy_from_slice(&((len / 512) as u32).to_le_bytes());
    };
    entry(0, 0x83, start1, ext.len(), &mut disk);
    entry(1, 0x83, start2, bt.len(), &mut disk);
    disk[510] = 0x55;
    disk[511] = 0xAA;
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-mbr.img", std::process::id()));
    std::fs::write(&path, &disk).unwrap();
    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![FsKind::Ext4, FsKind::Btrfs]);
    let src = Arc::new(Mutex::new(src));
    let set = volume::open_all_report(&src, &parts, &SubvolSelect::TopLevel).unwrap();
    assert_eq!(set.volumes.len(), 2);
    assert!(set.failed.is_empty());
    let _ = std::fs::remove_file(path);
}

/// Poškozený obraz: čtení pokračuje a poškození se hlásí (nic se nevynechá potichu).
#[test]
fn damaged_image_keeps_listing_and_reports_problems() {
    let mut vol = open(&unpack("ext4-damaged"), &SubvolSelect::TopLevel);
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
    // zbytek svazku se přečetl: 3000 souborů v `many` (minus ztracený blok), sparse.bin, hello.txt, ...
    assert!(files > 1500, "only {files} files were listed");
    let all = format!("{warnings:?} {failed_dirs:?} {failed_reads:?}");
    assert!(
        !failed_dirs.is_empty() || warnings.iter().any(|w| w.starts_with("/many")),
        "damaged directories were not reported: {all}"
    );
    assert!(all.contains("Case.txt") || all.contains("big.bin"), "damaged files were not reported: {all}");
    // ostatní soubory ve stejném adresáři jsou čitelné: selhat smí jen soubory s vynulovaným inodem
    assert!(
        failed_reads.iter().all(|r| r.starts_with("/big.bin") || r.starts_with("/names/Case.txt")),
        "files in intact blocks must stay readable: {} failures, first: {:?}",
        failed_reads.len(),
        failed_reads.first()
    );
    assert_eq!(failed_reads.len(), 2, "{failed_reads:?}");
}
