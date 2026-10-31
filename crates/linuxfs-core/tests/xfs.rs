//! XFS: obrazy v `tests/fixtures` vyrobil `mkfs.xfs` a naplnilo je jádro Linuxu přes mount (viz `tools/make-xfs-fixtures.sh`),
//! `*.manifest` obsahuje typ, velikost, mtime a sha256 podle toho, co hlásí jádro. Čtečka se musí shodovat na každém bajtu
//! u v5 (4 KiB bloky, reflink, rmapbt, bigtime), v5 s malými bloky a víceblokovými adresáři, v4 (bez CRC, bez typu v adresáři)
//! a u svazku s nečistým žurnálem, který se přehrává jen v paměti.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
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

/// Rozbalí `*.img.gz` jako ŘÍDKÝ soubor: megabajty samých nul se přeskočí. Obrazy XFS mají přes 300 MB (méně mkfs.xfs
/// nevyrobí), ale skoro všechno jsou nuly, takže na disku zabírají jen desítky MB.
fn unpack_sparse(gz: &Path, out: &Path) {
    let mut dec = flate2::read::GzDecoder::new(File::open(gz).unwrap());
    let mut f = File::create(out).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    loop {
        let mut n = 0;
        while n < buf.len() {
            let r = dec.read(&mut buf[n..]).unwrap();
            if r == 0 {
                break;
            }
            n += r;
        }
        if n == 0 {
            break;
        }
        if buf[..n].iter().any(|&b| b != 0) {
            f.seek(SeekFrom::Start(pos)).unwrap();
            f.write_all(&buf[..n]).unwrap();
        }
        pos += n as u64;
        if n < buf.len() {
            break;
        }
    }
    f.set_len(pos).unwrap();
}

/// Rozbalí obraz do dočasného souboru (jednou na proces a obraz).
fn unpack(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("linuxfs-test-{}-{name}.img", std::process::id()));
    // Testy běží paralelně: rozbalování se serializuje, aby jiný test nečetl napůl zapsaný obraz.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.exists() {
        unpack_sparse(&fixture(&format!("{name}.img.gz")), &out);
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
    /// `None` = manifest mtime neuvádí (v4 obraz plněný přes `mkfs.xfs -p` nese čas výroby, ne čas ze zdroje).
    mtime: Option<i64>,
    sha: String,
}

fn open(path: &Path) -> LinuxVolume {
    let (src, parts, _) = detect::locate(path).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::Xfs);
    let src = Arc::new(Mutex::new(src));
    let mut v = volume::open_all(&src, &parts, &SubvolSelect::TopLevel).unwrap();
    v.remove(0).vol
}

/// Projde celý strom a vrátí cesta -> položka; obsah souborů hashuje.
fn walk(vol: &mut LinuxVolume, check_mtime: bool) -> BTreeMap<String, Entry> {
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
            let mtime = if check_mtime { Some(n.mtime.unwrap()) } else { None };
            out.insert(path, Entry { kind, size: n.size, mtime, sha });
        }
    }
    out
}

fn manifest(name: &str) -> BTreeMap<String, Entry> {
    let bytes = std::fs::read(fixture(&format!("{name}.manifest"))).unwrap();
    let mut m = BTreeMap::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let mut p = line.splitn(5, '\t');
        let kind = p.next().unwrap().chars().next().unwrap();
        let size = p.next().unwrap().parse().unwrap();
        let mtime = match p.next().unwrap() {
            "-" => None,
            t => Some(t.parse().unwrap()),
        };
        let sha = p.next().unwrap().to_string();
        m.insert(p.next().unwrap().to_string(), Entry { kind, size, mtime, sha });
    }
    m
}

/// Porovná celý strom s manifestem; vrací otevřený svazek (pro další kontroly).
fn compare(name: &str, label: &str) -> LinuxVolume {
    let mut vol = open(&unpack(name));
    assert_eq!(vol.kind(), FsKind::Xfs);
    assert_eq!(vol.name(), label);
    let want = manifest(name);
    let check_mtime = want.values().any(|e| e.mtime.is_some());
    let got = walk(&mut vol, check_mtime);
    for (p, w) in &want {
        match got.get(p) {
            None => panic!("{name}: missing in the plugin view: {p:?}"),
            Some(g) => assert_eq!(g, w, "{name}: {p:?}"),
        }
    }
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    assert!(extra.is_empty(), "{name}: extra entries: {extra:?}");
    assert!(want.len() > 3000, "{name}: the manifest looks too small ({} entries)", want.len());
    vol
}

fn info_of(vol: &mut LinuxVolume) -> String {
    vol.info_text_for_tests().to_string()
}

#[test]
fn v5_matches_the_kernel() {
    let mut vol = compare("xfs-v5", "LinuxFsV5");
    let info = info_of(&mut vol);
    for want in ["Version:      v5", "Block size:   4096 B", "reflink", "rmapbt", "bigtime", "finobt", "Log:          internal", "The log is clean."] {
        assert!(info.contains(want), "{want:?} missing in:\n{info}");
    }
    // časy před rokem 1970 a za rokem 2038 (bigtime) a soubory, kvůli kterým je obraz vyroben tak, jak je
    let want = manifest("xfs-v5");
    assert_eq!(want["old-1950.txt"].mtime, Some(-631_152_000));
    assert_eq!(want["future-2100.txt"].mtime, Some(4_102_444_800));
    assert!(want["fragmented.bin"].size > 20_000_000 && want.contains_key("reflink.bin") && want.contains_key("prealloc.bin"));
}

#[test]
fn v5_with_small_blocks_and_multiblock_directories_matches_the_kernel() {
    let mut vol = compare("xfs-v5-small", "LinuxFsV5Sm");
    let info = info_of(&mut vol);
    assert!(info.contains("Block size:   1024 B") && info.contains("Allocation groups: 4 x"), "{info}");
    assert!(!info.contains("reflink") && !info.contains("bigtime"), "{info}");
}

#[test]
fn v4_without_checksums_matches_the_source() {
    let mut vol = compare("xfs-v4", "LinuxFsV4");
    let info = info_of(&mut vol);
    assert!(info.contains("Version:      v4") && info.contains("Block size:   2048 B") && info.contains("inode size 256 B"), "{info}");
}

#[test]
fn info_file_symlinks_and_hidden_specials() {
    let mut vol = open(&unpack("xfs-v5"));
    let root = vol.root();
    let list = vol.list_directory(root).unwrap();
    let info = list.iter().find(|n| n.name == volume::info_name_for(FsKind::Xfs)).unwrap();
    assert_eq!(info.name, "!XFS-INFO.txt");
    let mut buf = Vec::new();
    vol.read_file_to(info.id, &mut buf).unwrap();
    assert_eq!(buf.len() as u64, info.size);
    let text = String::from_utf8(buf).unwrap();
    assert!(text.contains("Label:        LinuxFsV5") && text.contains("UUID:"), "{text}");
    // FIFO a zařízení se nezobrazují
    assert!(!list.iter().any(|n| n.name == "fifo" || n.name == "chardev"));
    // symlinky: krátký (v inodu), 126 B (v inodu 512 B) a 406 B (v samostatném bloku s hlavičkou)
    for (name, len) in [("link-rel", 9usize), ("link-long", 126), ("link-xlong", 406)] {
        let link = list.iter().find(|n| n.name == name).unwrap();
        assert_eq!(link.kind, NodeKind::Symlink);
        assert_eq!(link.size, len as u64, "{name}");
        let mut t = Vec::new();
        vol.read_file_to(link.id, &mut t).unwrap();
        assert_eq!(t.len(), len, "{name}");
    }
    let mut t = Vec::new();
    vol.read_file_to(list.iter().find(|n| n.name == "link-rel").unwrap().id, &mut t).unwrap();
    assert_eq!(t, b"hello.txt");
    // adresář nejde číst jako soubor
    let d = list.iter().find(|n| n.name == "deep").unwrap();
    assert!(vol.read_file_to(d.id, &mut Vec::new()).is_err());
    // krátký adresář (v inodu) a velký adresář přes mnoho bloků
    let few = list.iter().find(|n| n.name == "few").unwrap();
    assert_eq!(vol.list_directory(few.id).unwrap().len(), 3);
    let many = list.iter().find(|n| n.name == "many").unwrap();
    assert_eq!(vol.list_directory(many.id).unwrap().len(), 3000);
    // hard link má stejné číslo inodu jako původní soubor
    let h = list.iter().find(|n| n.name == "hello.txt").unwrap();
    let h1 = list.iter().find(|n| n.name == "hard1.txt").unwrap();
    assert_eq!(h.id, h1.id);
    // neexistující číslo položky je chyba, ne pád
    let bogus = volume::NodeId { tree: 7, ino: 1 };
    assert!(vol.read_file_to(bogus, &mut Vec::new()).is_err());
    assert!(vol.list_directory(bogus).is_err());
}

/// Nečistý žurnál: změny, které zůstaly jen v žurnálu, se přehrají v paměti a zdroj se nezmění.
#[test]
fn dirty_log_is_replayed_in_memory_and_the_image_is_untouched() {
    let img = unpack("xfs-dirty");
    let before = std::fs::metadata(&img).unwrap().modified().unwrap();
    let mut vol = open(&img);
    let info = info_of(&mut vol);
    assert!(info.contains("The log has not been replayed") && info.contains("in memory only"), "{info}");
    let got = walk(&mut vol, true);
    let want = manifest("xfs-dirty");
    for (p, w) in &want {
        match got.get(p) {
            None => panic!("missing after the log replay: {p:?}"),
            Some(g) => assert_eq!(g, w, "{p:?}"),
        }
    }
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    assert!(extra.is_empty(), "extra entries: {extra:?}");
    // právě to, co se změnilo až po posledním zápisu metadat
    assert!(got.contains_key("renamed.txt") && !got.contains_key("hello.txt"), "rename in the log");
    assert!(!got.contains_key("empty.bin") && !got.contains_key("names/case.txt"), "removal in the log");
    assert!(got.contains_key("late/f40.txt") && got.contains_key("late/sub/synced.txt") && got.contains_key("late/link-to-renamed"), "creation in the log");
    assert_eq!(std::fs::metadata(&img).unwrap().modified().unwrap(), before, "the source must not be modified");
}

/// MBR se třemi oddíly s nenulovým posunem (ext4, XFS, btrfs): čtečka musí adresovat vždy uvnitř oddílu.
#[test]
fn partition_table_with_ext4_xfs_and_btrfs() {
    let xfs = unpack("xfs-v5-small");
    let ext = std::fs::read(unpack_other("ext4")).unwrap();
    let bt = std::fs::read(unpack_other("btrfs-small")).unwrap();
    let xfs_len = std::fs::metadata(&xfs).unwrap().len();
    let sectors = |bytes: u64| bytes.div_ceil(512);
    let start1 = 2048u64 * 512;
    let start2 = start1 + sectors(ext.len() as u64) * 512 + 2048 * 512;
    let start3 = start2 + sectors(xfs_len) * 512 + 2048 * 512;
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-mbr-xfs.img", std::process::id()));
    let mut disk = File::create(&path).unwrap();
    disk.set_len(start3 + bt.len() as u64 + 512 * 2048).unwrap();
    disk.seek(SeekFrom::Start(start1)).unwrap();
    disk.write_all(&ext).unwrap();
    // XFS (320 MB, skoro samé nuly) se kopíruje řídce
    let mut f = File::open(&xfs).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        if buf[..n].iter().any(|&b| b != 0) {
            disk.seek(SeekFrom::Start(start2 + pos)).unwrap();
            disk.write_all(&buf[..n]).unwrap();
        }
        pos += n as u64;
    }
    disk.seek(SeekFrom::Start(start3)).unwrap();
    disk.write_all(&bt).unwrap();
    let mut mbr = [0u8; 512];
    for (i, (start, len)) in [(start1, ext.len() as u64), (start2, xfs_len), (start3, bt.len() as u64)].into_iter().enumerate() {
        let o = 446 + 16 * i;
        mbr[o + 4] = 0x83;
        mbr[o + 8..o + 12].copy_from_slice(&((start / 512) as u32).to_le_bytes());
        mbr[o + 12..o + 16].copy_from_slice(&(sectors(len) as u32).to_le_bytes());
    }
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    disk.seek(SeekFrom::Start(0)).unwrap();
    disk.write_all(&mbr).unwrap();
    drop(disk);

    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![FsKind::Ext4, FsKind::Xfs, FsKind::Btrfs]);
    let src = Arc::new(Mutex::new(src));
    let set = volume::open_all_report(&src, &parts, &SubvolSelect::TopLevel).unwrap();
    assert_eq!(set.volumes.len(), 3);
    assert!(set.failed.is_empty(), "{:?}", set.failed);
    assert_eq!(set.volumes[1].label, "Partition 2 - LinuxFsV5Sm");
    let mut x = set.volumes.into_iter().nth(1).unwrap().vol;
    assert_eq!(walk(&mut x, true).len(), manifest("xfs-v5-small").len());
    let _ = std::fs::remove_file(path);
}

/// Obraz jiného souborového systému z fixtures (ext4, btrfs): rozbalí se obyčejně, jsou malé.
fn unpack_other(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("linuxfs-test-{}-xfs-{name}.img", std::process::id()));
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.exists() {
        let gz = File::open(fixture(&format!("{name}.img.gz"))).unwrap();
        io::copy(&mut flate2::read::GzDecoder::new(gz), &mut File::create(&out).unwrap()).unwrap();
    }
    out
}

/// Zkrácený obraz se pozná jako XFS; otevření buď selže s hláškou, nebo se chyby hlásí při čtení (nikdy panika a nikdy tichá ztráta).
#[test]
fn truncated_image_is_detected_and_reports_errors() {
    let src_img = unpack("xfs-v5");
    let path = std::env::temp_dir().join(format!("linuxfs-test-{}-xfs-trunc.img", std::process::id()));
    {
        let mut inp = File::open(&src_img).unwrap();
        let mut out = File::create(&path).unwrap();
        let keep = 100u64 << 20; // z 320 MB zbývá 100 MB: alokační skupiny 1–3 chybí
        let mut buf = vec![0u8; 1 << 20];
        let mut pos = 0u64;
        while pos < keep {
            let n = inp.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            if buf[..n].iter().any(|&b| b != 0) {
                out.seek(SeekFrom::Start(pos)).unwrap();
                out.write_all(&buf[..n]).unwrap();
            }
            pos += n as u64;
        }
        out.set_len(keep).unwrap();
    }
    let (src, parts, _) = detect::locate(&path).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].kind, FsKind::Xfs);
    let src = Arc::new(Mutex::new(src));
    match volume::open_part(&src, &parts[0], &SubvolSelect::TopLevel) {
        Err(e) => assert!(!e.to_string().is_empty()),
        Ok(mut vol) => {
            // otevřelo se: výpis celého stromu smí hlásit chyby, ale nesmí spadnout
            let mut stack = vec![vol.root()];
            let mut errors = 0usize;
            while let Some(d) = stack.pop() {
                match vol.list_directory_ex(d) {
                    Ok(l) => {
                        errors += l.warnings.len();
                        for n in l.nodes {
                            if n.is_dir() {
                                stack.push(n.id);
                            } else if n.id != volume::INFO_ID && vol.read_file_to(n.id, &mut io::sink()).is_err() {
                                errors += 1;
                            }
                        }
                    }
                    Err(_) => errors += 1,
                }
            }
            assert!(errors > 0, "a truncated volume must report that data is missing");
        }
    }
    let _ = std::fs::remove_file(path);
}

/// Poškozený obraz: čtení pokračuje a poškození se hlásí (nic se nevynechá potichu).
#[test]
fn damaged_image_keeps_listing_and_reports_problems() {
    let mut vol = open(&unpack("xfs-damaged"));
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
                    if n.id != volume::INFO_ID {
                        if let Err(e) = vol.read_file_to(n.id, &mut io::sink()) {
                            failed_reads.push(format!("{path}: {e}"));
                        }
                    }
                }
            }
        }
    }
    let all = format!("{warnings:?} {failed_dirs:?} {failed_reads:?}");
    // zbytek svazku se přečetl: adresář s vadným blokem se nevypíše celý (3000 souborů v `many` je pryč) a strom pod zničeným
    // inodem `level3` je nedostupný, ale soubory v kořeni, `names` a `few` se vypíšou i přečtou
    assert!(files >= 25, "only {files} files were listed: {all}");
    // zničené adresáře: /many (první datový blok) a level3 (inod); oba se ohlásí jako nečitelné
    assert!(
        failed_dirs.iter().any(|d| d.starts_with("/many:")) && failed_dirs.iter().any(|d| d.starts_with("/deep/level1/level2/level3:")),
        "damaged directories were not reported: {all}"
    );
    assert_eq!(failed_dirs.len(), 2, "only the damaged directories may fail: {all}");
    // nečitelné inody položek se ohlásí už při výpisu rodiče a položka se přesto ukáže
    for name in ["big.bin", "Case.txt", "level3"] {
        assert!(warnings.iter().any(|w| w.contains(name) && w.contains("inode unreadable")), "{name} not reported: {all}");
    }
    assert!(failed_reads.iter().any(|r| r.starts_with("/big.bin")) && failed_reads.iter().any(|r| r.starts_with("/names/Case.txt")), "{all}");
    // ostatní soubory ve stejných adresářích jsou čitelné: selhat smí jen soubory se zničeným inodem
    assert!(
        failed_reads.iter().all(|r| r.starts_with("/big.bin") || r.starts_with("/names/Case.txt")),
        "files with intact inodes must stay readable: {} failures, first: {:?}",
        failed_reads.len(),
        failed_reads.first()
    );
}
