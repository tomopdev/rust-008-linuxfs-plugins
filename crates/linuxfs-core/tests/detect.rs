//! Hledání svazků v tabulkách oddílů: GPT (sektor 512 i 4096), MBR (primární i logické oddíly), odkazy `.linuxfsdev`.

use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use linuxfs_core::btrfs::SubvolSelect;
use linuxfs_core::detect::{self, FsKind};

fn unpack(name: &str) -> PathBuf {
    let out =
        std::env::temp_dir().join(format!("linuxfs-detect-{}-{name}.img", std::process::id()));
    // Testy běží paralelně: rozbalování se serializuje, aby jiný test nečetl napůl zapsaný obraz.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.exists() {
        let gz = File::open(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
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

fn put(disk: &mut File, at: u64, bytes: &[u8]) {
    disk.seek(SeekFrom::Start(at)).unwrap();
    disk.write_all(bytes).unwrap();
}

fn copy_image(disk: &mut File, at: u64, image: &Path) {
    disk.seek(SeekFrom::Start(at)).unwrap();
    io::copy(&mut File::open(image).unwrap(), disk).unwrap();
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("linuxfs-detect-{}-{name}", std::process::id()))
}

fn gpt_disk(path: &Path, image: &Path, sector: u64, names: &[&str]) {
    let len = fs::metadata(image).unwrap().len();
    let mut disk = File::create(path).unwrap();
    disk.set_len((1 << 20) + names.len() as u64 * (len + (1 << 20)))
        .unwrap();
    let mut hdr = vec![0u8; 92];
    hdr[..8].copy_from_slice(b"EFI PART");
    hdr[72..80].copy_from_slice(&2u64.to_le_bytes());
    hdr[80..84].copy_from_slice(&8u32.to_le_bytes());
    hdr[84..88].copy_from_slice(&128u32.to_le_bytes());
    put(&mut disk, sector, &hdr);
    for (i, name) in names.iter().enumerate() {
        let start = (1 << 20) + i as u64 * (len + (1 << 20));
        let mut e = vec![0u8; 128];
        e[..16].copy_from_slice(&[
            0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47,
            0x7D, 0xE4,
        ]); // Linux filesystem
        e[32..40].copy_from_slice(&(start / sector).to_le_bytes());
        e[40..48].copy_from_slice(&((start + len) / sector - 1).to_le_bytes());
        for (k, u) in name.encode_utf16().enumerate() {
            e[56 + k * 2..58 + k * 2].copy_from_slice(&u.to_le_bytes());
        }
        put(&mut disk, 2 * sector + i as u64 * 128, &e);
        copy_image(&mut disk, start, image);
    }
}

fn mbr_entry(typ: u8, start_lba: u64, sectors: u64) -> [u8; 16] {
    let mut e = [0u8; 16];
    e[4] = typ;
    e[8..12].copy_from_slice(&(start_lba as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
    e
}

#[test]
fn bare_image_is_found_at_offset_zero() {
    let img = unpack("btrfs-small");
    let (_, parts, sel) = detect::locate(&img).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!((parts[0].start, parts[0].kind), (0, FsKind::Btrfs));
    assert_eq!(sel, SubvolSelect::TopLevel);
}

#[test]
fn gpt_with_both_sector_sizes_and_partition_names() {
    let img = unpack("btrfs-small");
    for sector in [512u64, 4096] {
        let disk = temp(&format!("gpt{sector}.img"));
        gpt_disk(&disk, &img, sector, &["system", "data"]);
        let (_, parts, _) = detect::locate(&disk).unwrap();
        let names: Vec<&str> = parts.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(names, ["system", "data"], "sector {sector}");
        assert_eq!(parts[0].start, 1 << 20);
        fs::remove_file(&disk).unwrap();
    }
}

#[test]
fn mbr_primary_and_logical_partitions() {
    let img = unpack("btrfs-small");
    let len = fs::metadata(&img).unwrap().len();
    let sectors = len / 512;
    let disk_path = temp("mbr.img");
    let mut disk = File::create(&disk_path).unwrap();
    let p1 = 2048u64; // primární
    let ext = p1 + sectors + 2048; // rozšířený oddíl
    let l1 = ext + 2048; // první logický (EBR v `ext`)
    let ebr2 = ext + sectors + 4096; // druhý EBR (relativně k `ext`)
    let l2 = ebr2 + 2048;
    disk.set_len((l2 + sectors + 2048) * 512).unwrap();

    let mut mbr = vec![0u8; 512];
    mbr[446..462].copy_from_slice(&mbr_entry(0x83, p1, sectors));
    mbr[462..478].copy_from_slice(&mbr_entry(0x0F, ext, l2 + sectors - ext));
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    put(&mut disk, 0, &mbr);
    copy_image(&mut disk, p1 * 512, &img);

    let mut ebr1 = vec![0u8; 512];
    ebr1[446..462].copy_from_slice(&mbr_entry(0x83, l1 - ext, sectors)); // vůči tomuto EBR (leží na `ext`)
    ebr1[462..478].copy_from_slice(&mbr_entry(0x05, ebr2 - ext, 2048 + sectors));
    ebr1[510] = 0x55;
    ebr1[511] = 0xAA;
    put(&mut disk, ext * 512, &ebr1);
    copy_image(&mut disk, l1 * 512, &img);

    let mut ebr2b = vec![0u8; 512];
    ebr2b[446..462].copy_from_slice(&mbr_entry(0x83, l2 - ebr2, sectors));
    ebr2b[510] = 0x55;
    ebr2b[511] = 0xAA;
    put(&mut disk, ebr2 * 512, &ebr2b);
    copy_image(&mut disk, l2 * 512, &img);
    drop(disk);

    let (_, parts, _) = detect::locate(&disk_path).unwrap();
    let starts: Vec<u64> = parts.iter().map(|p| p.start / 512).collect();
    assert_eq!(starts, [p1, l1, l2]);
    fs::remove_file(&disk_path).unwrap();
}

#[test]
fn devlink_selects_partition_offset_and_subvolume() {
    let img = unpack("btrfs-small");
    let disk = temp("link.img");
    gpt_disk(&disk, &img, 512, &["one", "two"]);
    let link = temp("x.linuxfsdev");

    fs::write(
        &link,
        format!(
            "device = {}\npartition = 2\nsubvol = @home\n",
            disk.display()
        ),
    )
    .unwrap();
    let (_, parts, sel) = detect::locate(&link).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].label, "two");
    assert_eq!(sel, SubvolSelect::Path("@home".into()));

    fs::write(
        &link,
        format!("device = {}\noffset = {}\n", disk.display(), 1 << 20),
    )
    .unwrap();
    assert_eq!(detect::locate(&link).unwrap().1.len(), 1);

    fs::write(
        &link,
        format!("device = {}\noffset = 4096\n", disk.display()),
    )
    .unwrap();
    assert!(detect::locate(&link).unwrap().1.is_empty());

    fs::write(&link, "bogus line").unwrap();
    assert!(matches!(
        detect::locate(&link),
        Err(detect::LocateError::Config(_))
    ));
    fs::remove_file(&disk).unwrap();
    fs::remove_file(&link).unwrap();
}

#[test]
fn junk_is_not_a_volume() {
    let f = temp("junk.img");
    fs::write(&f, vec![7u8; 300_000]).unwrap();
    assert!(detect::locate(&f).unwrap().1.is_empty());
    // magie na správném místě, ale špatná vlastní adresa => není to superblok
    let mut v = vec![0u8; 300_000];
    v[65536 + 0x40..65536 + 0x48].copy_from_slice(b"_BHRfS_M");
    fs::write(&f, v).unwrap();
    assert!(detect::locate(&f).unwrap().1.is_empty());
    fs::remove_file(&f).unwrap();
}
