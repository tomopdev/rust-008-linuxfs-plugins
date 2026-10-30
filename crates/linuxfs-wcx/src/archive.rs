//! Platform-neutral plugin core: opens volumes (btrfs on image files, raw partitions or whole disks), walks them lazily one
//! header at a time (what `ReadHeaderExW` needs) and extracts files (`ProcessFileW`). Read-only.

use std::collections::{HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::detect::{self, LocateError};
use crate::ffi::{self, ProcessDataProcW};
use crate::report::ProblemLog;
use crate::util;
use crate::volume::{self, LinuxVolume, Node, NodeId, NodeKind, VolumeError};

/// One item reported to Total Commander.
#[derive(Debug, Clone)]
pub struct Entry {
    vol: usize,
    /// Address of the item inside its volume.
    id: NodeId,
    /// Path shown to Total Commander (`\` separated, Windows-safe names).
    pub disp: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time, unix seconds (UTC).
    pub mtime: Option<i64>,
}

struct Job {
    vol: usize,
    id: NodeId,
    disp: String,
}

/// Why `Archive::open` failed.
#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    /// Link file (`.linuxfsdev`) is malformed.
    Config(String),
    /// Nothing that looks like a supported file system was found.
    NoVolume,
    /// A volume was found but could not be parsed.
    Fs(VolumeError),
}

impl OpenError {
    pub fn code(&self) -> i32 {
        match self {
            OpenError::Io(_) => ffi::E_EOPEN,
            OpenError::Config(_) => ffi::E_BAD_ARCHIVE,
            OpenError::NoVolume => ffi::E_UNKNOWN_FORMAT,
            OpenError::Fs(VolumeError::Unsupported(_)) => ffi::E_UNKNOWN_FORMAT,
            OpenError::Fs(e) if e.is_io() => ffi::E_EREAD,
            OpenError::Fs(_) => ffi::E_BAD_ARCHIVE,
        }
    }
}

pub struct Archive {
    /// Name of the opened source, copied into every header.
    pub arc_name: String,
    vols: Vec<LinuxVolume>,
    work: Vec<Job>,
    pending: VecDeque<Entry>,
    current: Option<Entry>,
    listed_any: bool,
    /// Directories already walked (a damaged file system must not send the walk in circles).
    seen: HashSet<(usize, NodeId)>,
    /// Everything that could not be listed (damaged directories, unreadable entries, partitions that did not open).
    /// The walk always continues; the list is shown (window + text file) when the archive is closed.
    pub list_problems: ProblemLog,
    /// Items that could not be extracted (read or write errors); Total Commander offers skip / abort itself,
    /// this list only makes sure nothing is lost silently.
    pub extract_problems: ProblemLog,
    /// `PK_OM_LIST` or `PK_OM_EXTRACT` from `OpenArchiveW`.
    pub mode: i32,
    pub progress: Option<ProcessDataProcW>,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive, OpenError> {
        let (src, parts, select) = detect::locate(path).map_err(|e| match e {
            LocateError::Io(e) => OpenError::Io(e),
            LocateError::Config(m) => OpenError::Config(m),
        })?;
        if parts.is_empty() {
            return Err(OpenError::NoVolume);
        }
        let src = Arc::new(Mutex::new(src));
        let set = volume::open_all_report(&src, &parts, &select).map_err(OpenError::Fs)?;
        let mut list_problems = ProblemLog::default();
        for (place, msg) in &set.failed {
            list_problems.add(place.clone(), format!("the volume was skipped, it could not be opened: {msg}"));
        }
        let (labels, vols): (Vec<String>, Vec<LinuxVolume>) =
            set.volumes.into_iter().map(|o| (o.label, o.vol)).unzip();

        let mut work = Vec::new();
        let mut pending = VecDeque::new();
        if vols.len() == 1 {
            work.push(Job {
                vol: 0,
                id: vols[0].root(),
                disp: String::new(),
            });
        } else {
            // Several volumes: one top-level folder per volume.
            let mut names = util::NameSet::default();
            for (i, label) in labels.iter().enumerate() {
                let disp = names.unique(util::sanitize_component(label));
                pending.push_back(Entry {
                    vol: i,
                    id: vols[i].root(),
                    disp,
                    is_dir: true,
                    size: 0,
                    mtime: None,
                });
            }
        }

        Ok(Archive {
            arc_name: path.to_string_lossy().into_owned(),
            vols,
            work,
            pending,
            current: None,
            listed_any: false,
            seen: HashSet::new(),
            list_problems,
            extract_problems: ProblemLog::default(),
            mode: ffi::PK_OM_LIST,
            progress: None,
        })
    }

    /// Next item of the walk, `Ok(None)` at the end. Directories are reported before their contents.
    pub fn next_header(&mut self) -> Result<Option<Entry>, VolumeError> {
        loop {
            if let Some(e) = self.pending.pop_front() {
                if e.is_dir {
                    self.work.push(Job {
                        vol: e.vol,
                        id: e.id,
                        disp: e.disp.clone(),
                    });
                }
                self.current = Some(e.clone());
                return Ok(Some(e));
            }
            let Some(job) = self.work.pop() else {
                self.current = None;
                return Ok(None);
            };
            if !self.seen.insert((job.vol, job.id)) {
                continue;
            }
            match self.vols[job.vol].list_directory_ex(job.id) {
                Ok(l) => {
                    self.listed_any = true;
                    self.list_problems.add_warnings(&job.disp, l.warnings);
                    self.enqueue(&job, l.nodes);
                }
                // Nothing listed yet: the volume is unusable, report it.
                Err(e) if !self.listed_any => {
                    self.list_problems
                        .add(job.disp.clone(), format!("the directory could not be listed: {e}"));
                    return Err(e);
                }
                // Damaged subdirectory: record it, keep the rest of the volume usable.
                Err(e) => self.list_problems.add(
                    job.disp.clone(),
                    format!("the directory could not be listed, its contents are missing: {e}"),
                ),
            }
        }
    }

    fn enqueue(&mut self, job: &Job, mut list: Vec<Node>) {
        list.sort_by_cached_key(|d| d.name.to_lowercase());
        let mut names = util::NameSet::default();
        for d in list {
            let shown = names.unique(util::sanitize_component(&d.name));
            let is_dir = d.kind == NodeKind::Dir;
            self.pending.push_back(Entry {
                vol: job.vol,
                id: d.id,
                disp: if job.disp.is_empty() {
                    shown
                } else {
                    format!("{}\\{}", job.disp, shown)
                },
                is_dir,
                size: if is_dir { 0 } else { d.size },
                mtime: d.mtime,
            });
        }
    }

    /// `ProcessFile`: handle the item returned by the last `next_header`.
    /// Returns a WCX result code (0 = OK). Failures are also recorded in [`Archive::extract_problems`].
    pub fn process(&mut self, op: i32, dest: Option<&Path>) -> i32 {
        let Some(entry) = self.current.clone() else {
            return ffi::E_BAD_DATA;
        };
        let (code, why) = self.process_entry(&entry, op, dest);
        if code != ffi::E_OK && code != ffi::E_EABORTED {
            self.extract_problems.add(entry.disp.clone(), why);
        }
        code
    }

    fn process_entry(&mut self, entry: &Entry, op: i32, dest: Option<&Path>) -> (i32, String) {
        if op == ffi::PK_SKIP {
            return (ffi::E_OK, String::new());
        }
        if entry.is_dir {
            if op == ffi::PK_EXTRACT {
                let Some(d) = dest else {
                    return (ffi::E_ECREATE, "no destination path".into());
                };
                if let Err(e) = fs::create_dir_all(util::long_path(d.to_path_buf())) {
                    return (ffi::E_ECREATE, format!("cannot create the folder {}: {e}", d.display()));
                }
            }
            return (ffi::E_OK, String::new());
        }

        let extract_to = if op == ffi::PK_EXTRACT {
            let Some(d) = dest else {
                return (ffi::E_ECREATE, "no destination path".into());
            };
            let d = util::long_path(d.to_path_buf());
            if let Some(parent) = d.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match File::create(&d) {
                Ok(f) => Some((d, f)),
                Err(e) => {
                    return (ffi::E_ECREATE, format!("cannot create the file {}: {e}", d.display()));
                }
            }
        } else {
            None
        };

        let mut sink = ProgressWriter {
            file: extract_to
                .as_ref()
                .map(|(_, f)| BufWriter::with_capacity(1 << 20, f)),
            cb: self.progress,
            name: ffi::to_wide_nul(&entry.disp),
            aborted: false,
            write_failed: false,
        };
        let read = self.vols[entry.vol].read_file_to(entry.id, &mut sink);
        let flushed = sink.flush();

        let (code, why) = match read {
            Ok(_) if flushed.is_err() => (ffi::E_EWRITE, format!("write error: {}", flushed.unwrap_err())),
            Ok(_) => (ffi::E_OK, String::new()),
            Err(_) if sink.aborted => (ffi::E_EABORTED, String::new()),
            Err(e) if sink.write_failed => (ffi::E_EWRITE, format!("write error: {e}")),
            Err(e) if e.is_io() => (ffi::E_EREAD, format!("read error: {e}")),
            Err(e) => (ffi::E_BAD_DATA, format!("damaged data: {e}")),
        };
        drop(sink);

        if let Some((path, file)) = extract_to {
            if code == ffi::E_OK {
                if let Some(t) = entry.mtime {
                    let _ = file.set_modified(util::unix_to_system_time(t));
                }
            } else {
                drop(file);
                let _ = fs::remove_file(path); // never leave a truncated file behind
            }
        }
        (code, why)
    }

    /// Called when the archive is closed: saves what went wrong to text files in `%TEMP%` and passes a short summary
    /// to `show` (a message box). Listing problems are reported when the archive was opened for listing, extraction
    /// problems when it was opened for extraction (Total Commander lists the archive again for every extraction, so
    /// the same listing problems are not repeated there).
    pub fn finish(&self, show: &mut dyn FnMut(&str)) {
        let listing = self.mode == ffi::PK_OM_LIST;
        let mut texts = Vec::new();
        if !self.list_problems.is_empty() {
            let file = self
                .list_problems
                .write_file("list", "problems while listing the archive", &self.arc_name);
            if listing {
                texts.push(self.list_problems.window_text("occurred while listing; the rest of the volume is shown", file.as_deref()));
            }
        }
        if !self.extract_problems.is_empty() {
            let file = self
                .extract_problems
                .write_file("unpack", "problems while extracting", &self.arc_name);
            texts.push(self.extract_problems.window_text("occurred while extracting; the other files were processed", file.as_deref()));
        }
        if !texts.is_empty() {
            show(&texts.join("\n\n-----\n\n"));
        }
    }
}

/// Writer that forwards to the destination file (if any) and reports progress
/// to Total Commander in at most 1 MiB steps.
struct ProgressWriter<'a> {
    file: Option<BufWriter<&'a File>>,
    cb: Option<ProcessDataProcW>,
    name: Vec<u16>,
    aborted: bool,
    write_failed: bool,
}

impl Write for ProgressWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(1 << 20);
        if let Some(f) = &mut self.file {
            if let Err(e) = f.write_all(&buf[..n]) {
                self.write_failed = true;
                return Err(e);
            }
        }
        if let Some(cb) = self.cb {
            // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call;
            // the callback pointer was supplied by Total Commander.
            let keep_going = unsafe { cb(self.name.as_mut_ptr(), n as i32) };
            if keep_going == 0 {
                self.aborted = true;
                return Err(io::Error::other("aborted by user"));
            }
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}
