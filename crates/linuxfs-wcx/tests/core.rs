//! Spouští jádro pluginu i exportované funkce WCX proti skutečnému btrfs obrazu vyrobenému jádrem Linuxu
//! (`../linuxfs-core/tests/fixtures`, viz `tools/make-btrfs-fixtures.sh`).

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use linuxfs_wcx::archive::Archive;
use linuxfs_wcx::ffi::*;
use linuxfs_wcx::*;
use sha2::{Digest, Sha256};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "linuxfs-wcx-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn unpack(name: &str) -> PathBuf {
    let out =
        std::env::temp_dir().join(format!("linuxfs-wcx-img-{}-{name}.img", std::process::id()));
    // Testy běží paralelně: rozbalování se serializuje, aby jiný test nečetl napůl zapsaný obraz.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.exists() {
        let gz = File::open(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../linuxfs-core/tests/fixtures")
                .join(format!("{name}.img.gz")),
        )
        .unwrap();
        io::copy(
            &mut flate2::read::GzDecoder::new(gz),
            &mut File::create(&out).unwrap(),
        )
        .unwrap();
    }
    out
}

/// Obraz XFS (přes 300 MB, skoro samé nuly) se rozbalí jako řídký soubor: nulové megabajty se přeskočí.
fn unpack_xfs(name: &str) -> PathBuf {
    use std::io::{Read, Seek, SeekFrom, Write};
    let out = std::env::temp_dir().join(format!("linuxfs-wcx-img-{}-{name}.img", std::process::id()));
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.exists() {
        let gz = File::open(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../linuxfs-core/tests/fixtures")
                .join(format!("{name}.img.gz")),
        )
        .unwrap();
        let mut dec = flate2::read::GzDecoder::new(gz);
        let mut f = File::create(&out).unwrap();
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
    out
}

fn list(a: &mut Archive) -> Vec<(String, bool, u64)> {
    let mut v = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        v.push((e.disp.clone(), e.is_dir, e.size));
    }
    v
}

fn wide(p: &Path) -> Vec<u16> {
    to_wide_nul(&p.to_string_lossy())
}

#[test]
fn list_shows_subvolumes_sanitised_and_unique_names() {
    let mut a = Archive::open(&unpack("btrfs-main")).unwrap();
    let items = list(&mut a);
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    for want in [
        "hello.txt",
        "!BTRFS-INFO.txt",
        "@",
        "@\\etc\\fstab",
        "@\\nested\\inner.txt",
        "@home\\note.txt",
        "@snapshots\\@-snap1\\etc\\fstab",
        "comp-zstd\\big.txt",
        "comp-lzo\\overwritten.txt",
        "names\\a_b_c.txt", // zakázané znaky
        "names\\_CON",      // rezervované jméno
        "names\\Case.txt",  // jména lišící se jen velikostí písmen dostanou unikátní jméno
        "names\\case (2).txt",
        "names\\Příliš žluťoučký kůň.txt",
    ] {
        assert!(
            names.contains(&want),
            "{want} missing in {:?}",
            &names[..names.len().min(60)]
        );
    }
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("fifo") || n.ends_with("sock"))
    );
    assert_eq!(
        items.iter().find(|i| i.0 == "huge-sparse.bin").unwrap().2,
        5 * 1024 * 1024 * 1024 + 123
    );
    assert!(items.iter().find(|i| i.0 == "@").unwrap().1);
}

#[test]
fn extract_compressed_files_matches_manifest() {
    let img = unpack("btrfs-main");
    let dir = temp_dir();
    let mut a = Archive::open(&img).unwrap();
    while let Some(e) = a.next_header().unwrap() {
        let wanted = [
            "comp-zstd\\big.txt",
            "comp-zlib\\overwritten.txt",
            "comp-lzo\\medium.txt",
            "hello.txt",
            "link-rel",
            "sparse.bin",
        ];
        if wanted.contains(&e.disp.as_str()) {
            let target = dir.join(e.disp.replace('\\', "/"));
            assert_eq!(a.process(PK_EXTRACT, Some(&target)), E_OK, "{}", e.disp);
        } else {
            assert_eq!(a.process(PK_SKIP, None), E_OK);
        }
    }
    assert_eq!(fs::read(dir.join("hello.txt")).unwrap(), b"Hello, btrfs!\n");
    assert_eq!(fs::read(dir.join("link-rel")).unwrap(), b"hello.txt");
    let manifest = String::from_utf8_lossy(
        &fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../linuxfs-core/tests/fixtures/btrfs-main.manifest"),
        )
        .unwrap(),
    )
    .into_owned();
    for rel in [
        "comp-zstd/big.txt",
        "comp-zlib/overwritten.txt",
        "comp-lzo/medium.txt",
        "sparse.bin",
    ] {
        let line = manifest
            .lines()
            .find(|l| l.ends_with(&format!("\t{rel}")))
            .unwrap();
        let sha = line.split('\t').nth(3).unwrap();
        let data = fs::read(dir.join(rel)).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&data)), sha, "{rel}");
    }
    // zachovaný čas změny
    let m = fs::metadata(dir.join("hello.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        m.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        1_709_210_096
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn devlink_chooses_a_subvolume() {
    let img = unpack("btrfs-main");
    let dir = temp_dir();
    let link = dir.join("home.linuxfsdev");
    fs::write(
        &link,
        format!("device = {}\nsubvol = @home\n", img.display()),
    )
    .unwrap();
    let mut a = Archive::open(&link).unwrap();
    let names: Vec<String> = list(&mut a).into_iter().map(|i| i.0).collect();
    assert_eq!(names, ["!BTRFS-INFO.txt", "note.txt"]); // seřazeno podle malých písmen: „!" je před písmeny
    fs::write(
        &link,
        format!("device = {}\nsubvol = nonexistent\n", img.display()),
    )
    .unwrap();
    assert!(Archive::open(&link).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn non_btrfs_is_rejected_with_unknown_format() {
    let dir = temp_dir();
    let f = dir.join("junk.img");
    fs::write(&f, vec![7u8; 300_000]).unwrap();
    let e = Archive::open(&f).err().expect("must fail");
    assert_eq!(e.code(), E_UNKNOWN_FORMAT);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn exported_wcx_functions() {
    let img = unpack("btrfs-small");
    let dir = temp_dir();
    unsafe {
        assert_eq!(GetPackerCaps(), PK_CAPS_BY_CONTENT);
        let w = wide(&img);
        assert_eq!(CanYouHandleThisFileW(w.as_ptr()), 1);
        let junk = dir.join("junk.img");
        fs::write(&junk, vec![1u8; 300_000]).unwrap();
        assert_eq!(CanYouHandleThisFileW(wide(&junk).as_ptr()), 0);
        let link = dir.join("a.linuxfsdev");
        fs::write(&link, "x").unwrap();
        assert_eq!(CanYouHandleThisFileW(wide(&link).as_ptr()), 1); // vždy přijmout; chyba se ohlásí při otevření

        let mut data = OpenArchiveDataW {
            ArcName: w.as_ptr() as *mut u16,
            OpenMode: PK_OM_EXTRACT,
            OpenResult: -1,
            CmtBuf: std::ptr::null_mut(),
            CmtBufSize: 0,
            CmtSize: 0,
            CmtState: 0,
        };
        let h = OpenArchiveW(&mut data);
        assert!(!h.is_null());
        assert_eq!(data.OpenResult, E_OK);

        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let mut seen = 0;
        let mut deepest = false;
        loop {
            let mut hd: Box<HeaderDataExW> = Box::new(std::mem::zeroed());
            let rc = ReadHeaderExW(h, &mut *hd);
            if rc == E_END_ARCHIVE {
                break;
            }
            assert_eq!(rc, E_OK);
            let n = hd.FileName.iter().position(|&c| c == 0).unwrap();
            let name = String::from_utf16_lossy(&hd.FileName[..n]);
            if name.ends_with("deepest.txt") {
                deepest = true;
                let full = wide(&out.join("deepest.txt"));
                assert_eq!(
                    ProcessFileW(h, PK_EXTRACT, std::ptr::null(), full.as_ptr()),
                    E_OK
                );
                assert_eq!(hd.UnpSize, 7);
            } else {
                assert_eq!(
                    ProcessFileW(h, PK_SKIP, std::ptr::null(), std::ptr::null()),
                    E_OK
                );
            }
            if name == "forced-zstd.txt" {
                assert_eq!(hd.UnpSize, 700_000);
            }
            seen += 1;
        }
        assert!(seen > 3000, "{seen}");
        assert!(deepest);
        assert_eq!(CloseArchive(h), E_OK);
    }
    assert_eq!(
        fs::read(dir.join("out").join("deepest.txt")).unwrap(),
        b"bottom\n"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn progress_callback_can_abort() {
    unsafe extern "system" fn stop(_name: *mut u16, _size: i32) -> i32 {
        0
    }
    let dir = temp_dir();
    let mut a = Archive::open(&unpack("btrfs-main")).unwrap();
    a.progress = Some(stop);
    let mut aborted = false;
    while let Some(e) = a.next_header().unwrap() {
        if e.disp != "big-plain.bin" {
            continue;
        }
        let target = dir.join("big.out");
        assert_eq!(a.process(PK_EXTRACT, Some(&target)), E_ABORTED_CODE);
        assert!(!target.exists(), "partial file must be removed");
        aborted = true;
        break;
    }
    assert!(aborted);
    fs::remove_dir_all(&dir).unwrap();
}

const E_ABORTED_CODE: i32 = E_EABORTED;

#[test]
fn ext4_lists_and_extracts() {
    let img = unpack("ext4");
    let dir = temp_dir();
    let mut a = Archive::open(&img).unwrap();
    let mut seen = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        seen.push(e.disp.clone());
        if e.disp == "hello.txt" || e.disp == "link-rel" {
            assert_eq!(a.process(PK_EXTRACT, Some(&dir.join(&e.disp))), E_OK);
        } else {
            assert_eq!(a.process(PK_SKIP, None), E_OK);
        }
    }
    assert!(seen.iter().any(|n| n == "!EXT4-INFO.txt") && seen.iter().any(|n| n == "names\\日本語.txt"), "{seen:?}");
    assert_eq!(fs::read(dir.join("hello.txt")).unwrap(), b"Hello, ext!\n");
    assert_eq!(fs::read(dir.join("link-rel")).unwrap(), b"hello.txt");
    assert!(a.list_problems.is_empty() && a.extract_problems.is_empty());
    let mut shown = Vec::new();
    a.finish(&mut |t| shown.push(t.to_string()));
    assert!(shown.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn xfs_lists_and_extracts() {
    let img = unpack_xfs("xfs-v5");
    let dir = temp_dir();
    let mut a = Archive::open(&img).unwrap();
    let mut seen = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        seen.push(e.disp.clone());
        if e.disp == "hello.txt" || e.disp == "link-rel" {
            assert_eq!(a.process(PK_EXTRACT, Some(&dir.join(&e.disp))), E_OK);
        } else {
            assert_eq!(a.process(PK_SKIP, None), E_OK);
        }
    }
    assert!(seen.iter().any(|n| n == "!XFS-INFO.txt") && seen.iter().any(|n| n == "names\\日本語.txt"), "{seen:?}");
    assert_eq!(fs::read(dir.join("hello.txt")).unwrap(), b"Hello, xfs!\n");
    assert_eq!(fs::read(dir.join("link-rel")).unwrap(), b"hello.txt");
    assert!(a.list_problems.is_empty() && a.extract_problems.is_empty());
    let mut shown = Vec::new();
    a.finish(&mut |t| shown.push(t.to_string()));
    assert!(shown.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn squashfs_lists_and_extracts() {
    let img = unpack("squashfs-xz");
    let dir = temp_dir();
    let mut a = Archive::open(&img).unwrap();
    let mut seen = Vec::new();
    while let Some(e) = a.next_header().unwrap() {
        seen.push(e.disp.clone());
        if e.disp == "hello.txt" || e.disp == "link-rel" {
            assert_eq!(a.process(PK_EXTRACT, Some(&dir.join(&e.disp))), E_OK);
        } else {
            assert_eq!(a.process(PK_SKIP, None), E_OK);
        }
    }
    assert!(seen.iter().any(|n| n == "!SQUASHFS-INFO.txt") && seen.iter().any(|n| n == "names\\日本語.txt"), "{seen:?}");
    assert_eq!(fs::read(dir.join("hello.txt")).unwrap(), b"Hello, squashfs!\n");
    assert_eq!(fs::read(dir.join("link-rel")).unwrap(), b"hello.txt");
    assert!(a.list_problems.is_empty() && a.extract_problems.is_empty());
    let mut shown = Vec::new();
    a.finish(&mut |t| shown.push(t.to_string()));
    assert!(shown.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

/// Obecný princip: chyba jednoho souboru / části svazku nezastaví zbytek; na konci okno + textový soubor.
#[test]
fn damaged_volume_is_listed_and_extracted_to_the_end_with_a_report() {
    let img = unpack("ext4-damaged");
    let dir = temp_dir();
    let _ = win::take_messages();

    // 1) výpis
    let mut a = Archive::open(&img).unwrap();
    let names = list(&mut a);
    assert!(names.len() > 1500, "{}", names.len());
    assert!(!a.list_problems.is_empty());
    let mut shown = Vec::new();
    a.finish(&mut |t| shown.push(t.to_string()));
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert!(shown[0].contains("many: ") && shown[0].contains("Full list:"), "{}", shown[0]);
    let log = std::env::temp_dir().join("linuxfs-errors-list.txt");
    let text = String::from_utf8_lossy(&fs::read(&log).unwrap()).into_owned();
    assert!(text.contains("big.bin") && text.contains("Problems:"), "{text}");

    // 2) rozbalení všeho: chybné soubory se vrátí jako chyba, ostatní se rozbalí
    let mut a = Archive::open(&img).unwrap();
    a.mode = PK_OM_EXTRACT;
    let (mut ok, mut bad) = (0, Vec::new());
    while let Some(e) = a.next_header().unwrap() {
        let code = a.process(PK_EXTRACT, Some(&dir.join(e.disp.replace('\\', "/"))));
        if code == E_OK {
            ok += 1;
        } else {
            bad.push((e.disp.clone(), code));
        }
    }
    assert!(ok > 1500, "{ok}");
    assert_eq!(bad.len(), 2, "{bad:?}");
    assert!(bad.iter().all(|(n, c)| (n == "big.bin" || n == "names\\Case.txt") && *c == E_BAD_DATA), "{bad:?}");
    assert!(!dir.join("big.bin").exists(), "no truncated file may remain");
    assert_eq!(fs::read(dir.join("hello.txt")).unwrap(), b"Hello, ext!\n");
    let mut shown = Vec::new();
    a.finish(&mut |t| shown.push(t.to_string()));
    assert_eq!(shown.len(), 1, "extraction mode reports extraction problems only: {shown:?}");
    assert!(shown[0].starts_with("2 problems") && shown[0].contains("big.bin"), "{}", shown[0]);
    let log = std::env::temp_dir().join("linuxfs-errors-unpack.txt");
    assert!(String::from_utf8_lossy(&fs::read(&log).unwrap()).contains("names\\Case.txt"));
    fs::remove_dir_all(&dir).unwrap();
}
