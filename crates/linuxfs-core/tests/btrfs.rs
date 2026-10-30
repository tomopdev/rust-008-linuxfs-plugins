//! Porovnání s referencí z jádra Linuxu: obrazy v `tests/fixtures` vyrobil skutečný btrfs (viz `tools/make-btrfs-fixtures.sh`)
//! a `*.manifest` obsahuje to, co o každé položce hlásilo jádro (typ, velikost, mtime, sha256, cesta).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use linuxfs_core::btrfs::SubvolSelect;
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

fn compare(name: &str) {
    let img = unpack(name);
    let mut vol = open(&img, &SubvolSelect::TopLevel);
    let got = walk(&mut vol, 1 << 30);
    let want = manifest(name);
    for (p, w) in &want {
        match got.get(p) {
            None => panic!("missing in the plugin view: {p:?}"),
            Some(g) => assert_eq!(g, w, "{p:?}"),
        }
    }
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    assert!(extra.is_empty(), "extra entries: {extra:?}");
    assert_eq!(got.len(), want.len());
}

#[test]
fn main_image_matches_the_kernel() {
    compare("btrfs-main");
}

#[test]
fn small_nodes_image_matches_the_kernel() {
    compare("btrfs-small");
}

#[test]
fn subvolume_listing_and_selection() {
    let img = unpack("btrfs-main");
    let vol = open(&img, &SubvolSelect::TopLevel);
    // reference z `btrfs subvolume list`
    let text = std::fs::read_to_string(fixture("btrfs-main.subvols")).unwrap();
    let mut want = Vec::new();
    let mut default = 0u64;
    for l in text.lines() {
        if let Some(d) = l.strip_prefix("default=") {
            default = d.parse().unwrap();
        } else {
            let mut p = l.splitn(3, '\t');
            let id: u64 = p.next().unwrap().parse().unwrap();
            let parent: u64 = p.next().unwrap().parse().unwrap();
            want.push((id, parent, p.next().unwrap().to_string()));
        }
    }
    let info = vol.info_text_for_tests();
    for (id, _, path) in &want {
        assert!(
            info.contains(&format!("ID {id:<6} {path}")),
            "{path} missing in:\n{info}"
        );
    }
    assert!(
        info.contains("[default]") && info.contains("[read-only]"),
        "{info}"
    );

    // výchozí podsvazek jako kořen = obsah `@` (bez jeho sourozenců)
    let mut v = open(&img, &SubvolSelect::Default);
    let names: Vec<String> = v
        .list_directory(v.root())
        .unwrap()
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert!(
        names.contains(&"etc".to_string()) && names.contains(&"nested".to_string()),
        "{names:?}"
    );
    assert!(!names.contains(&"hello.txt".to_string()));
    let mut v2 = open(&img, &SubvolSelect::Path("@home".into()));
    let names: Vec<String> = v2
        .list_directory(v2.root())
        .unwrap()
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert!(names.contains(&"note.txt".to_string()), "{names:?}");
    let mut v3 = open(
        &img,
        &SubvolSelect::Id(want.iter().find(|w| w.2 == "@home").unwrap().0),
    );
    assert_eq!(v3.list_directory(v3.root()).unwrap().len(), 2); // note.txt + info
    assert_eq!(default, want.iter().find(|w| w.2 == "@").unwrap().0);
}

#[test]
fn missing_subvolume_is_an_error() {
    let img = unpack("btrfs-main");
    let (src, parts, _) = detect::locate(&img).unwrap();
    let src = Arc::new(Mutex::new(src));
    assert!(volume::open_all(&src, &parts, &SubvolSelect::Path("nope".into())).is_err());
}

#[test]
fn info_file_and_symlink_contents() {
    let img = unpack("btrfs-main");
    let mut vol = open(&img, &SubvolSelect::TopLevel);
    let root = vol.root();
    let list = vol.list_directory(root).unwrap();
    let info = list.iter().find(|n| n.name == volume::INFO_NAME).unwrap();
    let mut buf = Vec::new();
    vol.read_file_to(info.id, &mut buf).unwrap();
    assert_eq!(buf.len() as u64, info.size);
    let text = String::from_utf8(buf).unwrap();
    assert!(
        text.contains("LinuxFsMain") && text.contains("compress_zstd"),
        "{text}"
    );

    let link = list.iter().find(|n| n.name == "link-rel").unwrap();
    assert_eq!(link.kind, NodeKind::Symlink);
    let mut t = Vec::new();
    vol.read_file_to(link.id, &mut t).unwrap();
    assert_eq!(t, b"hello.txt");
    // fifo, socket a zařízení se nezobrazují
    for hidden in ["fifo", "sock", "chardev"] {
        assert!(
            !list.iter().any(|n| n.name == hidden),
            "{hidden} should be hidden"
        );
    }
    // adresář se nedá číst jako soubor
    let d = list.iter().find(|n| n.name == "dir").unwrap();
    assert!(vol.read_file_to(d.id, &mut Vec::new()).is_err());
}
