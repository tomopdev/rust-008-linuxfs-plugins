#![allow(
    clippy::ptr_arg,
    clippy::manual_dangling_ptr,
    clippy::fn_to_numeric_cast
)]

//! Testuje plugin přes jeho skutečné ABI: mock „Salamanderu“ postavený ze stejných vtabulek (entry objekt, general
//! objekt, adresář, dialog průběhu, enumerátor) volá `SalamanderPluginEntry` a metody archiveru.
//! Obrazy btrfs jsou stejné fixtures jako v `linuxfs-core` (vyrobené skutečným jádrem Linuxu).

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use linuxfs_spl::spl::*;
use linuxfs_spl::win::{Answer, set_answer, take_messages};
use linuxfs_spl::{SalamanderPluginEntry, SalamanderPluginGetReqVer, SalamanderPluginGetSDKVer};

// ---------------------------------------------------------------------------
// mock CSalamanderGeneralAbstract (only the slots the plugin uses)
// ---------------------------------------------------------------------------

#[repr(C)]
struct MockGeneral {
    vtbl: *const usize,
}

unsafe extern "system" fn g_alloc(_this: *mut c_void, n: c_int) -> *mut c_void {
    let layout = Layout::from_size_align(n as usize + 16, 16).unwrap();
    unsafe {
        let p = alloc(layout);
        *(p as *mut usize) = n as usize + 16;
        p.add(16) as *mut c_void
    }
}
unsafe extern "system" fn g_free(_this: *mut c_void, p: *mut c_void) {
    if p.is_null() {
        return;
    }
    unsafe {
        let base = (p as *mut u8).sub(16);
        let size = *(base as *const usize);
        dealloc(base, Layout::from_size_align(size, 16).unwrap());
    }
}
unsafe extern "system" fn g_parent(_this: *mut c_void) -> HWND {
    null_mut()
}

fn general() -> *mut c_void {
    static G: OnceLock<usize> = OnceLock::new();
    *G.get_or_init(|| {
        let vt: &'static mut [usize; 40] = Box::leak(Box::new([0usize; 40]));
        vt[GENERAL_SLOT_GET_MSGBOX_PARENT] = g_parent as *const () as usize;
        vt[GENERAL_SLOT_ALLOC] = g_alloc as *const () as usize;
        vt[GENERAL_SLOT_FREE] = g_free as *const () as usize;
        Box::leak(Box::new(MockGeneral { vtbl: vt.as_ptr() })) as *mut MockGeneral as usize
    }) as *mut c_void
}

// ---------------------------------------------------------------------------
// mock CSalamanderPluginEntryAbstract / CSalamanderConnectAbstract
// ---------------------------------------------------------------------------

#[repr(C)]
struct MockEntry {
    vtbl: *const EntryVtbl,
    version: c_int,
    name: String,
    functions: u32,
    extensions: String,
    fs_name_null: bool,
}

unsafe extern "system" fn e_version(this: *mut Entry) -> c_int {
    unsafe { (*(this as *mut MockEntry)).version }
}
unsafe extern "system" fn e_parent(_this: *mut Entry) -> HWND {
    null_mut()
}
unsafe extern "system" fn e_set_basic(
    this: *mut Entry,
    name: *const c_char,
    functions: u32,
    _version: *const c_char,
    _copyright: *const c_char,
    _description: *const c_char,
    _reg: *const c_char,
    ext: *const c_char,
    fs: *const c_char,
) -> BOOL {
    unsafe {
        let m = &mut *(this as *mut MockEntry);
        m.name = CStr::from_ptr(name).to_string_lossy().into_owned();
        m.functions = functions;
        m.extensions = CStr::from_ptr(ext).to_string_lossy().into_owned();
        m.fs_name_null = fs.is_null();
    }
    TRUE
}
unsafe extern "system" fn e_general(_this: *mut Entry) -> *mut c_void {
    general()
}

static ENTRY_VTBL: EntryVtbl = EntryVtbl {
    GetVersion: e_version,
    GetParentWindow: e_parent,
    GetSalamanderDebug: null(),
    SetBasicPluginData: e_set_basic,
    GetSalamanderGeneral: e_general,
};

impl MockEntry {
    fn new(version: c_int) -> Box<MockEntry> {
        Box::new(MockEntry {
            vtbl: &ENTRY_VTBL,
            version,
            name: String::new(),
            functions: 0,
            extensions: String::new(),
            fs_name_null: false,
        })
    }
}

#[repr(C)]
struct MockConnect {
    vtbl: *const ConnectVtbl,
    calls: Vec<(String, BOOL, BOOL)>,
}
unsafe extern "system" fn c_add_archiver(
    this: *mut Connect,
    ext: *const c_char,
    edit: BOOL,
    upd: BOOL,
) {
    unsafe {
        (*(this as *mut MockConnect)).calls.push((
            CStr::from_ptr(ext).to_string_lossy().into_owned(),
            edit,
            upd,
        ));
    }
}
static CONNECT_VTBL: ConnectVtbl = ConnectVtbl {
    AddCustomPacker: null(),
    AddCustomUnpacker: null(),
    AddPanelArchiver: c_add_archiver,
};

// ---------------------------------------------------------------------------
// mock CSalamanderDirectoryAbstract
// ---------------------------------------------------------------------------

/// Decode ANSI bytes the way Salamander's host would (emulated code page on Linux).
fn ansi(b: &[u8]) -> String {
    linuxfs_spl::win::from_ansi(b)
}

#[derive(Debug, Clone)]
struct Add {
    is_dir: bool,
    parent: String,
    name: String,
    ext: String,
    size: u64,
    attr: u32,
}

#[repr(C)]
struct MockDir {
    vtbl: *const DirVtbl,
    adds: Vec<Add>,
    valid: u32,
    cleared: bool,
    names: Vec<*mut c_char>,
}

unsafe extern "system" fn d_clear(this: *mut Dir, _pd: *mut c_void) {
    unsafe { (*(this as *mut MockDir)).cleared = true }
}
unsafe extern "system" fn d_valid(this: *mut Dir, v: u32) {
    unsafe { (*(this as *mut MockDir)).valid = v }
}
unsafe extern "system" fn d_flags(_this: *mut Dir, _f: u32) {}
unsafe fn record(this: *mut Dir, path: *const c_char, fd: *mut FileData, is_dir: bool) -> BOOL {
    unsafe {
        let m = &mut *(this as *mut MockDir);
        let f = std::ptr::read_unaligned(fd);
        let name_bytes = CStr::from_ptr(f.Name).to_bytes();
        assert_eq!(
            f.name_len(),
            name_bytes.len(),
            "NameLen must equal strlen(Name)"
        );
        let name = ansi(name_bytes);
        assert_eq!((f.Bits >> 12) & 0xF, ICONOVERLAYINDEX_NOTUSED);
        assert!(f.DosName.is_null());
        assert!(
            f.Ext as usize >= f.Name as usize
                && f.Ext as usize <= f.Name as usize + name_bytes.len(),
            "Ext must point into Name"
        );
        m.adds.push(Add {
            is_dir,
            parent: ansi(CStr::from_ptr(path).to_bytes()),
            name,
            ext: ansi(CStr::from_ptr(f.Ext).to_bytes()),
            size: f.Size.value(),
            attr: f.Attr,
        });
        m.names.push(f.Name); // freed in Drop, like Salamander would
        TRUE
    }
}
unsafe extern "system" fn d_add_file(
    this: *mut Dir,
    path: *const c_char,
    fd: *mut FileData,
    _pd: *mut c_void,
) -> BOOL {
    unsafe { record(this, path, fd, false) }
}
unsafe extern "system" fn d_add_dir(
    this: *mut Dir,
    path: *const c_char,
    fd: *mut FileData,
    _pd: *mut c_void,
) -> BOOL {
    unsafe { record(this, path, fd, true) }
}
static DIR_VTBL: DirVtbl = DirVtbl {
    Clear: d_clear,
    SetValidData: d_valid,
    SetFlags: d_flags,
    AddFile: d_add_file,
    AddDir: d_add_dir,
};

impl MockDir {
    fn new() -> Box<MockDir> {
        Box::new(MockDir {
            vtbl: &DIR_VTBL,
            adds: vec![],
            valid: 0,
            cleared: false,
            names: vec![],
        })
    }
    fn as_dir(&mut self) -> *mut Dir {
        self as *mut MockDir as *mut Dir
    }
    fn find(&self, name: &str) -> &Add {
        self.adds
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("{name} not listed: {:?}", self.adds))
    }
}
impl Drop for MockDir {
    fn drop(&mut self) {
        for &n in &self.names {
            unsafe { g_free(null_mut(), n as *mut c_void) };
        }
    }
}

// ---------------------------------------------------------------------------
// mock CSalamanderForOperationsAbstract (progress dialog)
// ---------------------------------------------------------------------------

#[repr(C)]
struct MockOps {
    vtbl: *const OpsVtbl,
    opened: u32,
    closed: u32,
    two_bars: bool,
    total2: u64,
    added: u64,
    texts: Vec<String>,
    /// ProgressAddSize starts returning FALSE once this many bytes were reported.
    cancel_after: Option<u64>,
}
unsafe extern "system" fn o_open(this: *mut Ops, _t: *const c_char, two: BOOL, _p: HWND, _f: BOOL) {
    unsafe {
        let m = &mut *(this as *mut MockOps);
        m.opened += 1;
        m.two_bars = two != 0;
    }
}
unsafe extern "system" fn o_text(this: *mut Ops, txt: *const c_char, _d: BOOL) {
    unsafe {
        (*(this as *mut MockOps))
            .texts
            .push(CStr::from_ptr(txt).to_string_lossy().into_owned())
    }
}
unsafe extern "system" fn o_total(this: *mut Ops, _t1: *const QuadWord, t2: *const QuadWord) {
    unsafe {
        let m = &mut *(this as *mut MockOps);
        if *t2 != QuadWord::MAX && m.two_bars && m.total2 == 0 {
            m.total2 = (*t2).value();
        }
    }
}
unsafe extern "system" fn o_set_size(
    _this: *mut Ops,
    _a: *const QuadWord,
    _b: *const QuadWord,
    _d: BOOL,
) -> BOOL {
    TRUE
}
unsafe extern "system" fn o_add(this: *mut Ops, n: c_int, _d: BOOL) -> BOOL {
    unsafe {
        let m = &mut *(this as *mut MockOps);
        m.added += n as u64;
        match m.cancel_after {
            Some(limit) if m.added >= limit => FALSE,
            _ => TRUE,
        }
    }
}
unsafe extern "system" fn o_enable(_this: *mut Ops, _e: BOOL) {}
unsafe extern "system" fn o_hwnd(_this: *mut Ops) -> HWND {
    null_mut()
}
unsafe extern "system" fn o_close(this: *mut Ops) {
    unsafe { (*(this as *mut MockOps)).closed += 1 }
}
static OPS_VTBL: OpsVtbl = OpsVtbl {
    OpenProgressDialog: o_open,
    ProgressDialogAddText: o_text,
    ProgressSetTotalSize: o_total,
    ProgressSetSize: o_set_size,
    ProgressAddSize: o_add,
    ProgressEnableCancel: o_enable,
    ProgressGetHWND: o_hwnd,
    CloseProgressDialog: o_close,
};
impl MockOps {
    fn new() -> Box<MockOps> {
        Box::new(MockOps {
            vtbl: &OPS_VTBL,
            opened: 0,
            closed: 0,
            two_bars: false,
            total2: 0,
            added: 0,
            texts: vec![],
            cancel_after: None,
        })
    }
    fn as_ops(&mut self) -> *mut Ops {
        self as *mut MockOps as *mut Ops
    }
}

// ---------------------------------------------------------------------------
// mock SalEnumSelection
// ---------------------------------------------------------------------------

struct EnumState {
    items: Vec<(CString, bool, u64)>,
    pos: usize,
}
unsafe extern "system" fn enum_cb(
    _parent: HWND,
    enum_files: c_int,
    is_dir: *mut BOOL,
    size: *mut QuadWord,
    file_data: *mut *const FileData,
    param: *mut c_void,
    err: *mut c_int,
) -> *const c_char {
    unsafe {
        let st = &mut *(param as *mut EnumState);
        if enum_files == -1 {
            st.pos = 0;
            return null();
        }
        if !err.is_null() {
            *err = SALENUM_SUCCESS;
        }
        if !file_data.is_null() {
            *file_data = null();
        }
        let Some((name, dir, sz)) = st.items.get(st.pos) else {
            return null();
        };
        st.pos += 1;
        if !is_dir.is_null() {
            *is_dir = *dir as BOOL;
        }
        if !size.is_null() {
            *size = QuadWord::new(*sz);
        }
        name.as_ptr()
    }
}
fn enumerator(items: &[(&str, bool, u64)]) -> Box<EnumState> {
    Box::new(EnumState {
        items: items
            .iter()
            .map(|(n, d, s)| (CString::new(*n).unwrap(), *d, *s))
            .collect(),
        pos: 0,
    })
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "linuxfs-spl-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../linuxfs-core/tests/fixtures")
}

/// Rozbalí `btrfs-main` / `btrfs-small` do `dir` a vrátí cestu k obrazu.
fn unpack_image(dir: &Path, name: &str) -> PathBuf {
    let out = dir.join(format!("{name}.img"));
    let mut gz = flate2::read::GzDecoder::new(
        File::open(fixture_dir().join(format!("{name}.img.gz"))).unwrap(),
    );
    let mut f = File::create(&out).unwrap();
    std::io::copy(&mut gz, &mut f).unwrap();
    out
}

/// Disk s GPT (sektor 512) a jedním oddílem na každý obraz, oddíly po 1 MiB mezerami.
fn gpt_disk(path: &Path, image: &Path, labels: &[&str]) {
    let sector = 512u64;
    let len = fs::metadata(image).unwrap().len();
    let mut disk = File::create(path).unwrap();
    disk.set_len((1 << 20) + labels.len() as u64 * (len + (1 << 20)))
        .unwrap();
    let mut hdr = vec![0u8; 92];
    hdr[..8].copy_from_slice(b"EFI PART");
    hdr[72..80].copy_from_slice(&2u64.to_le_bytes());
    hdr[80..84].copy_from_slice(&8u32.to_le_bytes());
    hdr[84..88].copy_from_slice(&128u32.to_le_bytes());
    disk.seek(SeekFrom::Start(sector)).unwrap();
    disk.write_all(&hdr).unwrap();
    let mut data = Vec::new();
    File::open(image).unwrap().read_to_end(&mut data).unwrap();
    for (i, label) in labels.iter().enumerate() {
        let start = (1 << 20) + i as u64 * (len + (1 << 20));
        let mut e = vec![0u8; 128];
        e[..16].copy_from_slice(&[
            0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47,
            0x7D, 0xE4,
        ]);
        e[32..40].copy_from_slice(&(start / sector).to_le_bytes());
        e[40..48].copy_from_slice(&((start + len) / sector - 1).to_le_bytes());
        for (k, u) in label.encode_utf16().enumerate() {
            e[56 + k * 2..58 + k * 2].copy_from_slice(&u.to_le_bytes());
        }
        disk.seek(SeekFrom::Start(2 * sector + i as u64 * 128))
            .unwrap();
        disk.write_all(&e).unwrap();
        disk.seek(SeekFrom::Start(start)).unwrap();
        disk.write_all(&data).unwrap();
    }
}

/// The archiver interface as Salamander obtains it: entry -> plugin -> archiver.
fn archiver() -> &'static ArchiverVtbl {
    let mut e = MockEntry::new(103);
    unsafe {
        let obj = SalamanderPluginEntry(&mut *e as *mut MockEntry as *mut Entry);
        assert!(!obj.is_null());
        let pvt = &*(*(obj as *mut Obj<PluginVtbl>)).vtbl;
        let a = (pvt.GetInterfaceForArchiver)(obj);
        assert!(!a.is_null());
        &*(*(a as *const Obj<ArchiverVtbl>)).vtbl
    }
}

fn list(path: &PathBuf) -> (BOOL, Box<MockDir>) {
    let a = archiver();
    let mut dir = MockDir::new();
    let mut ops = MockOps::new();
    let c = CString::new(path.to_str().unwrap()).unwrap();
    let mut pd: *mut c_void = 1 as *mut c_void;
    let r = unsafe { (a.ListArchive)(null_mut(), ops.as_ops(), c.as_ptr(), dir.as_dir(), &mut pd) };
    assert!(pd.is_null(), "plugin must report no plugin data interface");
    (r, dir)
}

fn unpack(
    arc: &PathBuf,
    target: &PathBuf,
    root: &str,
    items: &[(&str, bool, u64)],
    ops: &mut MockOps,
) -> BOOL {
    let a = archiver();
    let c = CString::new(arc.to_str().unwrap()).unwrap();
    let t = CString::new(target.to_str().unwrap()).unwrap();
    let r = CString::new(root).unwrap();
    let mut st = enumerator(items);
    unsafe {
        (a.UnpackArchive)(
            null_mut(),
            ops.as_ops(),
            c.as_ptr(),
            null_mut(),
            t.as_ptr(),
            r.as_ptr(),
            enum_cb,
            &mut *st as *mut EnumState as *mut c_void,
        )
    }
}

fn manifest_sha(image: &str, path: &str) -> String {
    let text = String::from_utf8_lossy(
        &fs::read(fixture_dir().join(format!("{image}.manifest"))).unwrap(),
    )
    .into_owned();
    text.lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() == 5 && f[4] == path).then(|| f[3].to_string())
        })
        .unwrap_or_else(|| panic!("{path} is not in the manifest"))
}

fn sha_of(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(fs::read(p).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

const EXTENSIONS: &str = "btrfs;ext2;ext3;ext4;squashfs;sqfs;sfs;img;raw;dd;linuxfsdev";

#[test]
fn entry_points_and_connect() {
    assert_eq!(SalamanderPluginGetReqVer(), 102);
    assert_eq!(SalamanderPluginGetSDKVer(), 103);

    // příliš starý hostitel -> odmítnuto se zprávou
    let _ = take_messages();
    let mut old = MockEntry::new(101);
    let r = unsafe { SalamanderPluginEntry(&mut *old as *mut MockEntry as *mut Entry) };
    assert!(r.is_null());
    assert!(take_messages().iter().any(|m| m.contains("requires")));

    // Altap Salamander 4.0 (102) i Open Salamander 5.0 (103)
    for v in [102, 103] {
        let mut e = MockEntry::new(v);
        let obj = unsafe { SalamanderPluginEntry(&mut *e as *mut MockEntry as *mut Entry) };
        assert!(!obj.is_null());
        assert_eq!(e.functions, FUNCTION_PANELARCHIVERVIEW);
        assert_eq!(e.extensions, EXTENSIONS);
        assert!(e.fs_name_null);
        assert!(e.name.contains("btrfs"));

        unsafe {
            let pvt = &*(*(obj as *mut Obj<PluginVtbl>)).vtbl;
            let mut conn = MockConnect {
                vtbl: &CONNECT_VTBL,
                calls: vec![],
            };
            (pvt.Connect)(
                obj,
                null_mut(),
                &mut conn as *mut MockConnect as *mut Connect,
            );
            assert_eq!(conn.calls, vec![(EXTENSIONS.to_string(), FALSE, FALSE)]);
            assert_eq!(
                (pvt.GetInterfaceForArchiver)(obj),
                &linuxfs_spl::plugin::ARCHIVER_OBJ as *const Obj<ArchiverVtbl> as *mut c_void
            );
            assert!((pvt.GetInterfaceForViewer)(obj).is_null());
            assert!((pvt.GetInterfaceForFS)(obj).is_null());
            assert!((pvt.GetInterfaceForMenuExt)(obj).is_null());
            assert_eq!((pvt.Release)(obj, null_mut(), FALSE), TRUE);
        }
    }
}

#[test]
fn list_bare_volume_shows_subvolumes_and_directories() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");

    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert!(!d.cleared);
    assert_eq!(d.valid & VALID_DATA_SIZE, VALID_DATA_SIZE);
    assert_eq!(d.valid & 0x0002, 0, "DOS names are not provided");

    let hello = d.find("hello.txt");
    assert_eq!(
        (
            hello.size,
            hello.ext.as_str(),
            hello.attr,
            hello.parent.as_str()
        ),
        (14, "txt", FILE_ATTRIBUTE_ARCHIVE, "")
    );
    assert_eq!(d.find("big-plain.bin").size, 1_500_000);
    assert!(d.find("!BTRFS-INFO.txt").size > 0);
    assert_eq!(
        d.find("huge-sparse.bin").size,
        5 * 1024 * 1024 * 1024 + 123,
        "sizes above 4 GiB"
    );
    // podsvazky jsou obyčejné adresáře; vnořený podsvazek je uvnitř @
    for sub in ["@", "@home", "@snapshots"] {
        let a = d.find(sub);
        assert!(
            a.is_dir && a.attr == FILE_ATTRIBUTE_DIRECTORY && a.parent.is_empty(),
            "{a:?}"
        );
    }
    assert!(
        d.adds
            .iter()
            .any(|a| a.name == "inner.txt" && a.parent == "@\\nested")
    );
    assert!(
        d.adds
            .iter()
            .any(|a| a.name == "note.txt" && a.parent == "@home")
    );
    // snapshot je prázdný ve vnořeném podsvazku (nepatří mu)
    assert!(
        d.adds
            .iter()
            .any(|a| a.name == "fstab" && a.parent == "@snapshots\\@-snap1\\etc")
    );
    assert!(
        !d.adds
            .iter()
            .any(|a| a.parent == "@snapshots\\@-snap1\\nested" && !a.is_dir)
    );
    // zvláštní soubory (fifo, socket, zařízení) se nezobrazují
    for hidden in ["fifo", "sock", "chardev"] {
        assert!(
            !d.adds.iter().any(|a| a.name == hidden),
            "{hidden} must stay hidden"
        );
    }
    // adresář je ohlášen dřív než jeho obsah
    let dir_pos = d
        .adds
        .iter()
        .position(|a| a.is_dir && a.name == "@home")
        .unwrap();
    let file_pos = d
        .adds
        .iter()
        .position(|a| a.name == "note.txt" && a.parent == "@home")
        .unwrap();
    assert!(dir_pos < file_pos, "AddDir must precede AddFile into it");

    // jména v panelu jsou jen ANSI: kolize po převodu zůstanou rozlišitelné
    let names: Vec<&str> = d
        .adds
        .iter()
        .filter(|a| a.parent == "names")
        .map(|a| a.name.as_str())
        .collect();
    assert!(names.len() >= 14, "{names:?}");
    let mut lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    lower.sort();
    lower.dedup();
    assert_eq!(
        lower.len(),
        names.len(),
        "names must be unique ignoring case: {names:?}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_multi_volume_puts_dirs_before_their_files() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-small");
    let disk = dir.join("disk.img");
    gpt_disk(&disk, &img, &["one", "two"]);

    let (r, d) = list(&disk);
    assert_eq!(r, TRUE);
    let kinds: Vec<(bool, &str, &str)> = d
        .adds
        .iter()
        .map(|a| (a.is_dir, a.parent.as_str(), a.name.as_str()))
        .collect();
    for part in ["Partition 1 - one", "Partition 2 - two"] {
        let dir_pos = kinds
            .iter()
            .position(|k| *k == (true, "", part))
            .unwrap_or_else(|| panic!("{kinds:?}"));
        let file_pos = kinds
            .iter()
            .position(|k| *k == (false, part, "forced-zstd.txt"))
            .unwrap_or_else(|| panic!("{kinds:?}"));
        assert!(dir_pos < file_pos, "AddDir must precede AddFile into it");
    }
    assert_eq!(d.find("Partition 1 - one").attr, FILE_ATTRIBUTE_DIRECTORY);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn list_rejects_unknown_data_with_message() {
    let dir = temp_dir();
    let junk = dir.join("junk.img");
    fs::write(&junk, vec![7u8; 300_000]).unwrap();
    let _ = take_messages();
    let (r, _d) = list(&junk);
    assert_eq!(r, FALSE);
    assert!(take_messages().iter().any(|m| m.contains("no supported file system")));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_archive_extracts_real_names_and_compressed_files() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();

    // panel ukazuje ANSI jména, Salamander je vrací; "comp-zstd" je adresář, jeho soubory se vyjmenují
    let items = [
        ("hello.txt", false, 14),
        ("big-plain.bin", false, 1_500_000),
        ("comp-zstd", true, 0),
        ("comp-zstd\\big.txt", false, 1_500_000),
        ("comp-lzo", true, 0),
        ("comp-lzo\\overwritten.txt", false, 600_000),
        ("comp-zlib", true, 0),
        ("comp-zlib\\medium.txt", false, 300_000),
        ("link-rel", false, 9),
    ];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &out, "", &items, &mut ops), TRUE);

    assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"Hello, btrfs!\n");
    assert_eq!(
        sha_of(&out.join("big-plain.bin")),
        manifest_sha("btrfs-main", "big-plain.bin")
    );
    for p in [
        "comp-zstd/big.txt",
        "comp-lzo/overwritten.txt",
        "comp-zlib/medium.txt",
    ] {
        assert_eq!(sha_of(&out.join(p)), manifest_sha("btrfs-main", p), "{p}");
    }
    assert_eq!(fs::read(out.join("link-rel")).unwrap(), b"hello.txt"); // symlink je malý soubor s cílem

    assert_eq!((ops.opened, ops.closed, ops.two_bars), (1, 1, true));
    let expected_total: u64 = items.iter().map(|i| i.2 + 1).sum();
    assert_eq!(ops.total2, expected_total);
    assert!(ops.added >= 1_500_000 + 14);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_archive_respects_archive_root() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-small");
    let disk = dir.join("disk.img");
    gpt_disk(&disk, &img, &["one", "two"]);

    // root = složka: vyjmenovaná jména jsou relativní k ní
    let out1 = dir.join("out1");
    fs::create_dir_all(&out1).unwrap();
    let mut ops = MockOps::new();
    let items = [("forced-zstd.txt", false, 700_000)];
    assert_eq!(
        unpack(&disk, &out1, "Partition 2 - two", &items, &mut ops),
        TRUE
    );
    assert_eq!(
        sha_of(&out1.join("forced-zstd.txt")),
        manifest_sha("btrfs-small", "forced-zstd.txt")
    );

    // root = kořen archivu: cesta je součástí jména a adresáře se vytvoří
    let out2 = dir.join("out2");
    fs::create_dir_all(&out2).unwrap();
    let items = [
        ("Partition 1 - one", true, 0),
        ("Partition 1 - one\\forced-zstd.txt", false, 700_000),
    ];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&disk, &out2, "", &items, &mut ops), TRUE);
    assert_eq!(
        sha_of(&out2.join("Partition 1 - one").join("forced-zstd.txt")),
        manifest_sha("btrfs-small", "forced-zstd.txt")
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_info_and_nested_subvolume_files() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");
    let (_, d) = list(&img);
    let info = d.find("!BTRFS-INFO.txt").size;
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let items = [
        ("!BTRFS-INFO.txt", false, info),
        ("nested\\inner.txt", false, 21),
    ];
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &out, "", &items[..1], &mut ops), TRUE);
    let text = fs::read_to_string(out.join("!BTRFS-INFO.txt")).unwrap();
    assert_eq!(text.len() as u64, info);
    assert!(text.contains("LinuxFsMain"), "{text}");
    let mut ops = MockOps::new();
    assert_eq!(unpack(&img, &out, "@", &items[1..], &mut ops), TRUE);
    assert_eq!(
        fs::read(out.join("nested").join("inner.txt")).unwrap(),
        b"in nested subvolume\n"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unpack_one_file_uses_the_panel_name_or_the_rename() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");
    let a = archiver();
    let arc = CString::new(img.to_str().unwrap()).unwrap();

    let go = |name: &str, new_name: Option<&str>, target: &PathBuf| -> BOOL {
        let mut ops = MockOps::new();
        let n = CString::new(name).unwrap();
        let t = CString::new(target.to_str().unwrap()).unwrap();
        let nn = new_name.map(|s| CString::new(s).unwrap());
        let mut rn: BOOL = FALSE;
        unsafe {
            (a.UnpackOneFile)(
                null_mut(),
                ops.as_ops(),
                arc.as_ptr(),
                null_mut(),
                n.as_ptr(),
                null(),
                t.as_ptr(),
                nn.as_ref().map_or(null(), |c| c.as_ptr()),
                &mut rn,
            )
        }
    };

    let t1 = dir.join("t1");
    fs::create_dir_all(&t1).unwrap();
    assert_eq!(go("hello.txt", None, &t1), TRUE);
    assert_eq!(fs::read(t1.join("hello.txt")).unwrap(), b"Hello, btrfs!\n");

    let t2 = dir.join("t2");
    fs::create_dir_all(&t2).unwrap();
    assert_eq!(go("hello.txt", Some("_renamed.txt"), &t2), TRUE);
    assert_eq!(
        fs::read(t2.join("_renamed.txt")).unwrap(),
        b"Hello, btrfs!\n"
    );

    // neznámé jméno -> FALSE se zprávou
    let _ = take_messages();
    assert_eq!(go("nope.txt", None, &t2), FALSE);
    assert!(take_messages().iter().any(|m| m.contains("not found")));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_aborts_and_removes_the_partial_file() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();

    let mut ops = MockOps::new();
    ops.cancel_after = Some(1000);
    assert_eq!(
        unpack(
            &img,
            &out,
            "",
            &[
                ("big-plain.bin", false, 1_500_000),
                ("hello.txt", false, 14)
            ],
            &mut ops
        ),
        FALSE
    );
    assert!(
        !out.join("big-plain.bin").exists(),
        "partial file must be removed"
    );
    assert!(!out.join("hello.txt").exists(), "nothing after the cancel");
    assert_eq!(ops.closed, 1);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn overwrite_question_is_asked_once_and_honoured() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");
    let items = [("hello.txt", false, 14)];

    for (answer, expect_ok, expect_content) in [
        (Answer::No, TRUE, &b"old"[..]),
        (Answer::Yes, TRUE, &b"Hello, btrfs!\n"[..]),
        (Answer::Cancel, FALSE, &b"old"[..]),
    ] {
        let out = temp_dir();
        fs::write(out.join("hello.txt"), b"old").unwrap();
        set_answer(answer);
        let _ = take_messages();
        let mut ops = MockOps::new();
        assert_eq!(
            unpack(&img, &out, "", &items, &mut ops),
            expect_ok,
            "{answer:?}"
        );
        assert_eq!(
            fs::read(out.join("hello.txt")).unwrap(),
            expect_content,
            "{answer:?}"
        );
        assert_eq!(
            take_messages()
                .iter()
                .filter(|m| m.contains("already exists"))
                .count(),
            1
        );
        fs::remove_dir_all(&out).unwrap();
    }
    set_answer(Answer::Yes);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn devlink_selects_a_subvolume() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "btrfs-main");
    let link = dir.join("home.linuxfsdev");
    fs::write(&link, format!("path={}\nsubvol=@home\n", img.display())).unwrap();
    let (r, d) = list(&link);
    assert_eq!(r, TRUE);
    assert_eq!(d.find("note.txt").size, 10);
    assert!(
        !d.adds.iter().any(|a| a.name == "hello.txt"),
        "only the chosen subvolume is shown"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ext4_is_listed_and_unpacked() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "ext4");
    let _ = take_messages();
    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert!(take_messages().is_empty(), "an intact volume reports nothing");
    assert!(d.adds.iter().any(|a| !a.is_dir && a.name == "!EXT4-INFO.txt"));
    assert!(d.adds.iter().any(|a| a.is_dir && a.name == "deep"));
    let hello = d.adds.iter().find(|a| a.name == "hello.txt").unwrap();
    assert_eq!(hello.size, 12);

    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let mut ops = MockOps::new();
    let items = [("hello.txt", false, 12), ("big.bin", false, 1_500_000), ("link-rel", false, 9)];
    assert_eq!(unpack(&img, &out, "", &items, &mut ops), TRUE);
    assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"Hello, ext!\n");
    assert_eq!(sha_of(&out.join("big.bin")), manifest_sha("ext4", "big.bin"));
    assert_eq!(fs::read(out.join("link-rel")).unwrap(), b"hello.txt");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn squashfs_is_listed_and_unpacked() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "squashfs-zstd");
    let _ = take_messages();
    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert!(take_messages().is_empty(), "an intact volume reports nothing");
    assert!(d.adds.iter().any(|a| !a.is_dir && a.name == "!SQUASHFS-INFO.txt"));
    assert!(d.adds.iter().any(|a| a.is_dir && a.name == "deep"));
    let hello = d.adds.iter().find(|a| a.name == "hello.txt").unwrap();
    assert_eq!(hello.size, 17);

    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let mut ops = MockOps::new();
    let items = [("hello.txt", false, 17), ("big.bin", false, 400_000), ("link-rel", false, 9)];
    assert_eq!(unpack(&img, &out, "", &items, &mut ops), TRUE);
    assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"Hello, squashfs!\n");
    assert_eq!(sha_of(&out.join("big.bin")), manifest_sha("squashfs", "big.bin"));
    assert_eq!(fs::read(out.join("link-rel")).unwrap(), b"hello.txt");
    fs::remove_dir_all(&dir).unwrap();
}

/// Obecný princip: výpis i rozbalení pokračují po chybě a na konci je shrnutí v okně i v textovém souboru.
#[test]
fn damaged_volume_lists_everything_readable_and_reports_what_failed() {
    let dir = temp_dir();
    let img = unpack_image(&dir, "ext4-damaged");
    let _ = take_messages();
    let (r, d) = list(&img);
    assert_eq!(r, TRUE);
    assert!(d.adds.len() > 1500, "{}", d.adds.len());
    let msgs = take_messages();
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("problems") && msgs[0].contains("many: ") && msgs[0].contains("Full list:"), "{}", msgs[0]);
    let log = std::env::temp_dir().join("linuxfs-errors-list.txt");
    let text = String::from_utf8_lossy(&fs::read(&log).unwrap()).into_owned();
    assert!(text.contains("big.bin") && text.contains("Case.txt") && text.contains("deep\\level1\\level2\\level3"), "{text}");

    // rozbalení: dva soubory jsou rozbité, třetí se rozbalí; uživatel odpovídá „přeskočit“ (Yes)
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    set_answer(Answer::Yes);
    let mut ops = MockOps::new();
    let items = [
        ("big.bin", false, 1_500_000),
        ("hello.txt", false, 12),
        ("names\\Case.txt", false, 13),
        ("names", true, 0),
    ];
    assert_eq!(unpack(&img, &out, "", &items, &mut ops), TRUE);
    assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"Hello, ext!\n");
    assert!(!out.join("big.bin").exists() && !out.join("names/Case.txt").exists());
    let msgs = take_messages();
    let questions = msgs.iter().filter(|m| m.contains("Cannot unpack")).count();
    assert_eq!(questions, 2, "{msgs:?}");
    let summary = msgs.last().unwrap();
    assert!(summary.starts_with("2 problems") && summary.contains("big.bin") && summary.contains("Full list:"), "{summary}");
    let log = std::env::temp_dir().join("linuxfs-errors-unpack.txt");
    assert!(String::from_utf8_lossy(&fs::read(&log).unwrap()).contains("Case.txt"));
    fs::remove_dir_all(&dir).unwrap();
}
