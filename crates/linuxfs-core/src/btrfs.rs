//! Souborový systém btrfs jen pro čtení: podsvazky (včetně snapshotů), výpis adresářů, symlinky a obsah souborů
//! včetně komprese zlib / LZO / zstd, vložených (inline) extentů, děr, preallokovaných extentů a překrývajících se extentů.
//!
//! Struktury na disku (superblok, mapování chunků, B-stromy, položky) parsuje crate `btrfs-disk`; tady je nad ním tenká
//! synchronní vrstva, která dělá to, co jádro Linuxu při `readdir` / `read`:
//!
//! * adresář = položky `DIR_INDEX` (objectid adresáře); záznam míří buď na inode ve stejném stromu, nebo na kořen podsvazku
//!   (`ROOT_ITEM`), kterým se přejde do jiného stromu,
//! * soubor = položky `EXTENT_DATA` seřazené podle posunu v souboru, mezi nimi díry (čtou se jako nuly),
//! * strom `5` (FS_TREE) je „top-level" svazek, který obsahuje všechny podsvazky jako adresáře; to je výchozí pohled pluginů
//!   (jako po `mount -o subvolid=5`), konkrétní podsvazek lze zvolit v souboru `.linuxfsdev` (`subvol=`).
//!
//! Kontrolní součty dat se neověřují (plugin čte, neopravuje); poškozená data se projeví chybou dekomprese nebo
//! nesmyslným obsahem, stejně jako při čtení bez `csum`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use btrfs_disk::items::{
    CompressionType, DirItem, FileExtentBody, FileExtentItem, FileExtentType, InodeItem, RootItem,
    RootRef,
};
use btrfs_disk::raw;
use btrfs_disk::reader::{self, BlockReader, TreeBlockCache};
use btrfs_disk::superblock::{self, Superblock};
use btrfs_disk::tree::{KeyType, TreeBlock};

use crate::lzo;
use crate::source::Slice;
use crate::volume::{Node, NodeId, NodeKind, VolumeError};

/// Číslo stromu top-level svazku (`BTRFS_FS_TREE_OBJECTID`).
pub const FS_TREE: u64 = 5;
/// Objectid kořenového adresáře každého podsvazku (`BTRFS_FIRST_FREE_OBJECTID`).
pub const FIRST_INODE: u64 = 256;
/// Objectid „adresáře" kořenového stromu, v němž je uložen výchozí podsvazek (`BTRFS_ROOT_TREE_DIR_OBJECTID`).
const ROOT_TREE_DIR: u64 = 6;

const INODE_ITEM: u8 = raw::BTRFS_INODE_ITEM_KEY as u8;
const INODE_REF: u8 = raw::BTRFS_INODE_REF_KEY as u8;
const DIR_ITEM: u8 = raw::BTRFS_DIR_ITEM_KEY as u8;
const DIR_INDEX: u8 = raw::BTRFS_DIR_INDEX_KEY as u8;
const EXTENT_DATA: u8 = raw::BTRFS_EXTENT_DATA_KEY as u8;
const ROOT_ITEM: u8 = raw::BTRFS_ROOT_ITEM_KEY as u8;
const ROOT_BACKREF: u8 = raw::BTRFS_ROOT_BACKREF_KEY as u8;
const ROOT_REF: u8 = raw::BTRFS_ROOT_REF_KEY as u8;
/// Prázdný adresář, který zůstane ve snapshotu na místě vnořeného podsvazku (`BTRFS_EMPTY_SUBVOL_DIR_OBJECTID`).
const EMPTY_SUBVOL_DIR: u64 = 2;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// Nejvíc bajtů dat, které se najednou čtou z nekomprimovaného extentu.
const READ_PIECE: u64 = 4 << 20;
/// Kolik položek `EXTENT_DATA` se načte najednou při čtení souboru.
const EXTENT_BATCH: usize = 512;
/// Rozpočet cache stromových bloků v bajtech.
const CACHE_BYTES: usize = 32 << 20;

/// Klíč B-stromu `(objectid, type, offset)`; řazení odpovídá řazení na disku.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Key(u64, u8, u64);

fn key_of(k: btrfs_disk::tree::DiskKey) -> Key {
    Key(k.objectid, k.key_type.to_raw(), k.offset)
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// Cache stromových bloků (nejstarší vyhozený první).
struct NodeCache {
    max_entries: usize,
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    map: HashMap<u64, (Arc<TreeBlock>, u64)>,
    tick: u64,
}

impl NodeCache {
    fn new(nodesize: usize) -> Self {
        NodeCache {
            max_entries: (CACHE_BYTES / nodesize.max(4096)).max(64),
            inner: Mutex::default(),
        }
    }
}

impl TreeBlockCache for NodeCache {
    fn get(&self, addr: u64) -> Option<Arc<TreeBlock>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.tick += 1;
        let tick = g.tick;
        g.map.get_mut(&addr).map(|e| {
            e.1 = tick;
            e.0.clone()
        })
    }

    fn put(&self, addr: u64, block: Arc<TreeBlock>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.tick += 1;
        let tick = g.tick;
        if g.map.len() >= self.max_entries && !g.map.contains_key(&addr) {
            // vyhodit čtvrtinu nejstarších najednou, ať se hledání nejstaršího nedělá při každém vložení
            let mut ages: Vec<(u64, u64)> = g.map.iter().map(|(k, v)| (v.1, *k)).collect();
            ages.sort_unstable();
            for (_, k) in ages.into_iter().take(self.max_entries / 4 + 1) {
                g.map.remove(&k);
            }
        }
        g.map.insert(addr, (block, tick));
    }

    fn invalidate(&self, addr: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .remove(&addr);
    }
}

/// Který podsvazek se zobrazí jako kořen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SubvolSelect {
    /// Top-level svazek (strom 5): všechny podsvazky jsou v něm vidět jako adresáře.
    #[default]
    TopLevel,
    /// Výchozí podsvazek (to, co Linux připojí bez `subvol=`).
    Default,
    /// Podsvazek podle čísla.
    Id(u64),
    /// Podsvazek podle cesty od top-level svazku (například `@` nebo `@/nested`).
    Path(String),
}

/// Jeden podsvazek (včetně snapshotů).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubvolInfo {
    pub id: u64,
    /// Strom, v jehož adresáři podsvazek leží (0 u top-level svazku).
    pub parent: u64,
    /// Cesta od top-level svazku, `/` jako oddělovač (prázdná u top-level svazku).
    pub path: String,
    pub read_only: bool,
    pub is_default: bool,
}

/// Údaje o kořeni jednoho stromu (z `ROOT_ITEM`).
#[derive(Debug, Clone)]
struct Subvol {
    root_dirid: u64,
    read_only: bool,
    /// Čas změny kořenového adresáře podsvazku (unix sekundy).
    mtime: Option<i64>,
}

pub struct BtrfsVolume {
    reader: BlockReader<Slice>,
    sb: Superblock,
    tree_roots: BTreeMap<u64, (u64, u64)>,
    subvols: HashMap<u64, Subvol>,
    start: NodeId,
    sectorsize: usize,
    info: String,
    /// Naposledy rozbalený extent: (adresa na disku, velikost na disku, rozbalená data) – pro soubory, jejichž extenty
    /// se po částečném přepsání odkazují na tentýž velký extent víckrát.
    last_extent: Option<(u64, u64, Vec<u8>)>,
}

/// Jméno bitu volitelných vlastností (jen ty, které stojí za zmínku v informačním souboru).
fn incompat_names(flags: u64) -> Vec<String> {
    const NAMES: [(u32, &str); 16] = [
        (raw::BTRFS_FEATURE_INCOMPAT_MIXED_BACKREF, "mixed_backref"),
        (raw::BTRFS_FEATURE_INCOMPAT_DEFAULT_SUBVOL, "default_subvol"),
        (raw::BTRFS_FEATURE_INCOMPAT_MIXED_GROUPS, "mixed_groups"),
        (raw::BTRFS_FEATURE_INCOMPAT_COMPRESS_LZO, "compress_lzo"),
        (raw::BTRFS_FEATURE_INCOMPAT_COMPRESS_ZSTD, "compress_zstd"),
        (raw::BTRFS_FEATURE_INCOMPAT_BIG_METADATA, "big_metadata"),
        (raw::BTRFS_FEATURE_INCOMPAT_EXTENDED_IREF, "extended_iref"),
        (raw::BTRFS_FEATURE_INCOMPAT_RAID56, "raid56"),
        (
            raw::BTRFS_FEATURE_INCOMPAT_SKINNY_METADATA,
            "skinny_metadata",
        ),
        (raw::BTRFS_FEATURE_INCOMPAT_NO_HOLES, "no_holes"),
        (raw::BTRFS_FEATURE_INCOMPAT_METADATA_UUID, "metadata_uuid"),
        (raw::BTRFS_FEATURE_INCOMPAT_RAID1C34, "raid1c34"),
        (raw::BTRFS_FEATURE_INCOMPAT_ZONED, "zoned"),
        (raw::BTRFS_FEATURE_INCOMPAT_EXTENT_TREE_V2, "extent_tree_v2"),
        (
            raw::BTRFS_FEATURE_INCOMPAT_RAID_STRIPE_TREE,
            "raid_stripe_tree",
        ),
        (raw::BTRFS_FEATURE_INCOMPAT_SIMPLE_QUOTA, "simple_quota"),
    ];
    let mut v: Vec<String> = NAMES
        .iter()
        .filter(|(b, _)| flags & u64::from(*b) != 0)
        .map(|(_, n)| (*n).to_string())
        .collect();
    if flags & u64::from(raw::BTRFS_FEATURE_INCOMPAT_REMAP_TREE) != 0 {
        v.push("remap_tree".into());
    }
    let known: u64 = NAMES.iter().fold(0, |a, (b, _)| a | u64::from(*b))
        | u64::from(raw::BTRFS_FEATURE_INCOMPAT_REMAP_TREE);
    if flags & !known != 0 {
        v.push(format!("unknown(0x{:x})", flags & !known));
    }
    v
}

/// Vlastnosti, se kterými se nedá číst vůbec (jiné rozložení dat nebo adres, než jaké čtečka zná).
fn refuse_reason(incompat: u64) -> Option<&'static str> {
    if incompat & u64::from(raw::BTRFS_FEATURE_INCOMPAT_ZONED) != 0 {
        Some("zoned")
    } else if incompat & u64::from(raw::BTRFS_FEATURE_INCOMPAT_RAID_STRIPE_TREE) != 0 {
        Some("raid_stripe_tree")
    } else if incompat & u64::from(raw::BTRFS_FEATURE_INCOMPAT_REMAP_TREE) != 0 {
        Some("remap_tree")
    } else if incompat & u64::from(raw::BTRFS_FEATURE_INCOMPAT_EXTENT_TREE_V2) != 0 {
        Some("extent_tree_v2")
    } else {
        None
    }
}

fn human(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

fn corrupt(msg: impl Into<String>) -> VolumeError {
    VolumeError::Format(msg.into())
}

impl BtrfsVolume {
    /// Otevře btrfs v daném okně zdroje. `select` určuje, který podsvazek je kořenem.
    pub fn open(slice: Slice, select: &SubvolSelect) -> Result<Self, VolumeError> {
        let mut probe = slice;
        let sb0 = superblock::read_superblock(&mut probe, 0)?;
        if !sb0.magic_is_valid() {
            return Err(corrupt(
                "not a btrfs file system (the superblock magic is missing)",
            ));
        }
        if let Some(what) = refuse_reason(sb0.incompat_flags) {
            return Err(VolumeError::Unsupported(format!(
                "btrfs feature '{what}' is not supported"
            )));
        }
        // Primární superblok, při poškození kopie (64 MiB, 256 GiB); první, která dá použitelný strom, vyhrává.
        let mut last_err = None;
        let mut opened = None;
        for mirror in 0..superblock::SUPER_MIRROR_MAX {
            match reader::filesystem_open_mirror(probe.clone_window(), mirror) {
                Ok(o) => {
                    opened = Some(o);
                    break;
                }
                Err(e) => {
                    last_err.get_or_insert(e);
                }
            }
        }
        let Some(mut fs) = opened else {
            return Err(VolumeError::Io(last_err.unwrap_or_else(|| {
                std::io::Error::other("cannot open the btrfs file system")
            })));
        };
        let sectorsize = fs.superblock.sectorsize as usize;
        let nodesize = fs.superblock.nodesize as usize;
        fs.reader.set_cache(Some(
            Arc::new(NodeCache::new(nodesize)) as Arc<dyn TreeBlockCache>
        ));

        let mut vol = BtrfsVolume {
            reader: fs.reader,
            sb: fs.superblock,
            tree_roots: fs.tree_roots,
            subvols: HashMap::new(),
            start: NodeId {
                tree: FS_TREE,
                ino: FIRST_INODE,
            },
            sectorsize,
            info: String::new(),
            last_extent: None,
        };
        let start_tree = match select {
            SubvolSelect::TopLevel => FS_TREE,
            SubvolSelect::Default => vol.default_subvol()?,
            SubvolSelect::Id(id) => *id,
            SubvolSelect::Path(p) => {
                let want = p.trim_matches('/');
                if want.is_empty() {
                    FS_TREE
                } else {
                    vol.subvolumes()?
                        .into_iter()
                        .find(|s| s.path == want)
                        .map(|s| s.id)
                        .ok_or_else(|| VolumeError::NotFound(format!("subvolume '{want}'")))?
                }
            }
        };
        let sv = vol.subvol(start_tree)?;
        vol.start = NodeId {
            tree: start_tree,
            ino: sv.root_dirid,
        };
        vol.info = vol.build_info();
        Ok(vol)
    }

    /// Kořenový adresář zobrazeného podsvazku.
    pub fn root(&self) -> NodeId {
        self.start
    }

    pub fn label(&self) -> &str {
        &self.sb.label
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    /// Text virtuálního souboru s popisem souborového systému a seznamem podsvazků.
    pub fn info_text(&self) -> &str {
        &self.info
    }

    // -----------------------------------------------------------------------------------------------------------------
    // prohledávání B-stromů
    // -----------------------------------------------------------------------------------------------------------------

    fn tree_root(&self, tree: u64) -> Result<u64, VolumeError> {
        if tree == 1 {
            return Ok(self.sb.root);
        }
        self.tree_roots
            .get(&tree)
            .map(|v| v.0)
            .ok_or_else(|| VolumeError::NotFound(format!("tree {tree}")))
    }

    /// Projde položky stromu s klíči v `[min, max]` v rostoucím pořadí; `f` dostane klíč a data položky.
    fn visit(
        &mut self,
        root: u64,
        min: Key,
        max: Key,
        f: &mut dyn FnMut(Key, &[u8]) -> Flow,
    ) -> Result<(), VolumeError> {
        self.visit_block(root, min, max, f, 0).map(|_| ())
    }

    fn visit_block(
        &mut self,
        addr: u64,
        min: Key,
        max: Key,
        f: &mut dyn FnMut(Key, &[u8]) -> Flow,
        depth: u32,
    ) -> Result<Flow, VolumeError> {
        if depth > 16 {
            return Err(corrupt("the B-tree is too deep (damaged file system)"));
        }
        let block = self.reader.read_tree_block(addr)?;
        if block.header().bytenr != addr {
            return Err(corrupt(format!(
                "the tree block at {addr} claims to be at {} (damaged file system)",
                block.header().bytenr
            )));
        }
        match &*block {
            TreeBlock::Node { ptrs, .. } => {
                let start = ptrs
                    .partition_point(|p| key_of(p.key) <= min)
                    .saturating_sub(1);
                for p in &ptrs[start..] {
                    if key_of(p.key) > max {
                        break;
                    }
                    if self.visit_block(p.blockptr, min, max, f, depth + 1)? == Flow::Stop {
                        return Ok(Flow::Stop);
                    }
                }
            }
            TreeBlock::Leaf { items, .. } => {
                let start = items.partition_point(|it| key_of(it.key) < min);
                for (i, it) in items.iter().enumerate().skip(start) {
                    let k = key_of(it.key);
                    if k > max {
                        return Ok(Flow::Stop);
                    }
                    let data = block
                        .item_data(i)
                        .ok_or_else(|| corrupt("a leaf item points outside of its block"))?;
                    if f(k, data) == Flow::Stop {
                        return Ok(Flow::Stop);
                    }
                }
            }
        }
        Ok(Flow::Continue)
    }

    fn inode(&mut self, tree: u64, ino: u64) -> Result<Option<InodeItem>, VolumeError> {
        let root = self.tree_root(tree)?;
        let k = Key(ino, INODE_ITEM, 0);
        let mut found = None;
        self.visit(root, k, k, &mut |_, d| {
            found = InodeItem::parse(d);
            Flow::Stop
        })?;
        Ok(found)
    }

    /// Údaje o podsvazku (poslední `ROOT_ITEM` s daným číslem v kořenovém stromu).
    fn subvol(&mut self, tree: u64) -> Result<Subvol, VolumeError> {
        if let Some(s) = self.subvols.get(&tree) {
            return Ok(s.clone());
        }
        if !self.tree_roots.contains_key(&tree) {
            return Err(VolumeError::NotFound(format!("subvolume {tree}")));
        }
        let root = self.sb.root;
        let mut item = None;
        self.visit(
            root,
            Key(tree, ROOT_ITEM, 0),
            Key(tree, ROOT_ITEM, u64::MAX),
            &mut |_, d| {
                item = RootItem::parse(d);
                Flow::Continue // poslední (nejvyšší offset) vyhrává
            },
        )?;
        let item =
            item.ok_or_else(|| VolumeError::NotFound(format!("root item of subvolume {tree}")))?;
        let sv = Subvol {
            root_dirid: if item.root_dirid == 0 {
                FIRST_INODE
            } else {
                item.root_dirid
            },
            read_only: item
                .flags
                .contains(btrfs_disk::items::RootItemFlags::RDONLY),
            mtime: InodeItem::parse(&item.inode_data).and_then(|i| i64::try_from(i.mtime.sec).ok()),
        };
        self.subvols.insert(tree, sv.clone());
        Ok(sv)
    }

    /// Je podsvazek `child` opravdu podsvazkem adresáře `dir` (pod jménem `name`)?
    fn owns_subvol(&mut self, dir: NodeId, child: u64, name: &[u8]) -> Result<bool, VolumeError> {
        let root = self.sb.root;
        let k = Key(dir.tree, ROOT_REF, child);
        let mut owned = false;
        self.visit(root, k, k, &mut |_, d| {
            owned = RootRef::parse(d).is_some_and(|r| r.dirid == dir.ino && r.name == name);
            Flow::Stop
        })?;
        Ok(owned)
    }

    /// Výchozí podsvazek: položka `default` v „adresáři" kořenového stromu.
    pub fn default_subvol(&mut self) -> Result<u64, VolumeError> {
        let root = self.sb.root;
        let mut id = FS_TREE;
        self.visit(
            root,
            Key(ROOT_TREE_DIR, DIR_ITEM, 0),
            Key(ROOT_TREE_DIR, DIR_ITEM, u64::MAX),
            &mut |_, d| {
                for e in DirItem::parse_all(d) {
                    if e.name == b"default" && e.location.key_type == KeyType::RootItem {
                        id = e.location.objectid;
                    }
                }
                Flow::Continue
            },
        )?;
        Ok(id)
    }

    /// Cesta adresáře od kořene podsvazku (přes `INODE_REF`), bez počátečního lomítka; `None` při poškození.
    fn inode_path(&mut self, tree: u64, ino: u64) -> Result<Option<String>, VolumeError> {
        let root_dir = self.subvol(tree)?.root_dirid;
        let root = self.tree_root(tree)?;
        let mut parts: Vec<String> = Vec::new();
        let mut cur = ino;
        let mut guard = 0;
        while cur != root_dir {
            guard += 1;
            if guard > 4096 {
                return Ok(None);
            }
            let mut found: Option<(u64, String)> = None;
            self.visit(
                root,
                Key(cur, INODE_REF, 0),
                Key(cur, INODE_REF, u64::MAX),
                &mut |k, d| {
                    // btrfs_inode_ref: u64 index, u16 name_len, name
                    if d.len() >= 10 {
                        let n = u16::from_le_bytes([d[8], d[9]]) as usize;
                        if let Some(name) = d.get(10..10 + n) {
                            found = Some((k.2, String::from_utf8_lossy(name).into_owned()));
                        }
                    }
                    Flow::Stop
                },
            )?;
            let Some((parent, name)) = found else {
                return Ok(None);
            };
            parts.push(name);
            cur = parent;
        }
        parts.reverse();
        Ok(Some(parts.join("/")))
    }

    /// Všechny podsvazky a snapshoty (bez top-level svazku), seřazené podle cesty.
    pub fn subvolumes(&mut self) -> Result<Vec<SubvolInfo>, VolumeError> {
        let default = self.default_subvol()?;
        let ids: Vec<u64> = self
            .tree_roots
            .keys()
            .copied()
            .filter(|&i| i >= FIRST_INODE && i <= u64::MAX - 256)
            .collect();
        let root = self.sb.root;
        // id -> (rodičovský strom, adresář v něm, jméno)
        let mut parents: HashMap<u64, (u64, u64, String)> = HashMap::new();
        for &id in &ids {
            let mut r = None;
            self.visit(
                root,
                Key(id, ROOT_BACKREF, 0),
                Key(id, ROOT_BACKREF, u64::MAX),
                &mut |k, d| {
                    if let Some(rr) = RootRef::parse(d) {
                        r = Some((
                            k.2,
                            rr.dirid,
                            String::from_utf8_lossy(&rr.name).into_owned(),
                        ));
                    }
                    Flow::Stop
                },
            )?;
            if let Some(v) = r {
                parents.insert(id, v);
            }
        }
        let mut paths: HashMap<u64, Option<String>> = HashMap::new();
        let mut out = Vec::new();
        for &id in &ids {
            let path = self.subvol_path(id, &parents, &mut paths, 0)?;
            let read_only = self.subvol(id).map(|s| s.read_only).unwrap_or(false);
            out.push(SubvolInfo {
                id,
                parent: parents.get(&id).map_or(0, |p| p.0),
                path: path.unwrap_or_else(|| format!("<unreachable subvolume {id}>")),
                read_only,
                is_default: id == default,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    fn subvol_path(
        &mut self,
        id: u64,
        parents: &HashMap<u64, (u64, u64, String)>,
        memo: &mut HashMap<u64, Option<String>>,
        depth: u32,
    ) -> Result<Option<String>, VolumeError> {
        if id == FS_TREE {
            return Ok(Some(String::new()));
        }
        if let Some(p) = memo.get(&id) {
            return Ok(p.clone());
        }
        if depth > 64 {
            return Ok(None);
        }
        let Some((ptree, dirid, name)) = parents.get(&id).cloned() else {
            memo.insert(id, None);
            return Ok(None);
        };
        let base = self.subvol_path(ptree, parents, memo, depth + 1)?;
        let dir = self.inode_path(ptree, dirid)?;
        let path = match (base, dir) {
            (Some(b), Some(d)) => {
                let mut p = b;
                for part in [d.as_str(), name.as_str()] {
                    if !part.is_empty() {
                        if !p.is_empty() {
                            p.push('/');
                        }
                        p.push_str(part);
                    }
                }
                Some(p)
            }
            _ => None,
        };
        memo.insert(id, path.clone());
        Ok(path)
    }

    fn build_info(&mut self) -> String {
        let sb = self.sb.clone();
        let mut t = String::new();
        let _ = write!(t, "btrfs file system");
        if !sb.label.is_empty() {
            let _ = write!(t, " \"{}\"", sb.label);
        }
        let _ = write!(t, "\r\n\r\n");
        let _ = write!(t, "UUID:        {}\r\n", sb.fsid);
        let _ = write!(t, "Generation:  {}\r\n", sb.generation);
        let _ = write!(
            t,
            "Size:        {} (used {})\r\n",
            human(sb.total_bytes),
            human(sb.bytes_used)
        );
        let _ = write!(
            t,
            "Node size:   {} bytes, sector size {} bytes\r\n",
            sb.nodesize, sb.sectorsize
        );
        let _ = write!(t, "Checksum:    {:?}\r\n", sb.csum_type);
        let _ = write!(t, "Devices:     {}\r\n", sb.num_devices);
        let mut profiles = BTreeSet::new();
        for m in self.reader.chunk_cache().iter() {
            let kind = if m.chunk_type & u64::from(raw::BTRFS_BLOCK_GROUP_DATA) != 0 {
                "data"
            } else if m.chunk_type & u64::from(raw::BTRFS_BLOCK_GROUP_METADATA) != 0 {
                "metadata"
            } else {
                "system"
            };
            profiles.insert(format!("{kind} {:?}", m.profile()));
        }
        if !profiles.is_empty() {
            let _ = write!(
                t,
                "Profiles:    {}\r\n",
                profiles.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        let feats = incompat_names(sb.incompat_flags);
        if !feats.is_empty() {
            let _ = write!(t, "Features:    {}\r\n", feats.join(", "));
        }
        if sb.num_devices > 1 {
            let _ = write!(
                t,
                "\r\nThis file system spans {} devices. Only the device that was opened is read, so files whose data lives on\r\n\
                 the other devices cannot be read.\r\n",
                sb.num_devices
            );
        }
        let _ = write!(
            t,
            "\r\nSubvolumes (the top-level volume, ID 5, is the root of the listing unless a subvolume was chosen):\r\n"
        );
        match self.subvolumes() {
            Ok(list) if list.is_empty() => {
                let _ = write!(t, "  (none)\r\n");
            }
            Ok(list) => {
                for s in list {
                    let mut flags = Vec::new();
                    if s.is_default {
                        flags.push("default");
                    }
                    if s.read_only {
                        flags.push("read-only");
                    }
                    let _ = write!(
                        t,
                        "  ID {:<6} {}{}\r\n",
                        s.id,
                        s.path,
                        if flags.is_empty() {
                            String::new()
                        } else {
                            format!("   [{}]", flags.join(", "))
                        }
                    );
                }
            }
            Err(e) => {
                let _ = write!(t, "  (cannot be listed: {e})\r\n");
            }
        }
        let _ = write!(
            t,
            "\r\nRead-only: the plugin never writes. Checksums of data are not verified. Special files (FIFO, sockets, device nodes) are not shown;\r\nsymbolic links are shown as small files that contain the link target.\r\n"
        );
        t
    }

    // -----------------------------------------------------------------------------------------------------------------
    // adresáře
    // -----------------------------------------------------------------------------------------------------------------

    /// Položky adresáře. Podsvazek v adresáři se hlásí jako adresář, který vede do jiného stromu; speciální soubory
    /// (FIFO, sockety, zařízení) se vynechávají.
    pub fn list_directory(&mut self, dir: NodeId) -> Result<Vec<Node>, VolumeError> {
        let root = self.tree_root(dir.tree)?;
        let mut entries: Vec<DirItem> = Vec::new();
        self.visit(
            root,
            Key(dir.ino, DIR_INDEX, 0),
            Key(dir.ino, DIR_INDEX, u64::MAX),
            &mut |_, d| {
                entries.extend(DirItem::parse_all(d));
                Flow::Continue
            },
        )?;
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            match e.location.key_type {
                KeyType::InodeItem => {
                    let id = NodeId {
                        tree: dir.tree,
                        ino: e.location.objectid,
                    };
                    // Chybějící nebo nečitelný inode (poškození, osiřelá položka): položka se vynechá, adresář zůstane čitelný.
                    let Ok(Some(ino)) = self.inode(id.tree, id.ino) else {
                        continue;
                    };
                    let kind = match ino.mode & S_IFMT {
                        S_IFDIR => NodeKind::Dir,
                        S_IFREG => NodeKind::File,
                        S_IFLNK => NodeKind::Symlink,
                        _ => continue,
                    };
                    out.push(Node {
                        name,
                        kind,
                        size: if kind == NodeKind::Dir { 0 } else { ino.size },
                        mtime: i64::try_from(ino.mtime.sec).ok(),
                        id,
                    });
                }
                KeyType::RootItem => {
                    let tree = e.location.objectid;
                    let Ok(sv) = self.subvol(tree) else { continue }; // podsvazek smazaný, ale ještě ne vyčištěný
                    // Do podsvazku se vstoupí jen tehdy, když ho tento adresář opravdu vlastní (existuje `ROOT_REF`
                    // rodič -> podsvazek se stejným adresářem a jménem). Snapshot rodiče totiž zdědí záznam adresáře,
                    // ale podsvazek v něm nemá; jádro tam ukáže prázdný adresář a my také.
                    let owned = self.owns_subvol(dir, tree, &e.name)?;
                    let id = if owned {
                        NodeId {
                            tree,
                            ino: sv.root_dirid,
                        }
                    } else {
                        NodeId {
                            tree: dir.tree,
                            ino: EMPTY_SUBVOL_DIR,
                        }
                    };
                    // čas bere z kořenového adresáře podsvazku (jako stat), záloha je kopie inode v ROOT_ITEM
                    let mtime = match self.inode(tree, sv.root_dirid) {
                        Ok(Some(i)) => i64::try_from(i.mtime.sec).ok(),
                        _ => sv.mtime,
                    };
                    out.push(Node {
                        name,
                        kind: NodeKind::Dir,
                        size: 0,
                        mtime,
                        id,
                    });
                }
                _ => {}
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------------------------------------------------
    // obsah souborů
    // -----------------------------------------------------------------------------------------------------------------

    /// Pošle obsah souboru (nebo cíl symlinku) do `out`; vrací počet bajtů.
    pub fn read_file_to(&mut self, id: NodeId, out: &mut dyn Write) -> Result<u64, VolumeError> {
        let ino = self.inode(id.tree, id.ino)?.ok_or_else(|| {
            VolumeError::NotFound(format!("inode {} in tree {}", id.ino, id.tree))
        })?;
        match ino.mode & S_IFMT {
            S_IFREG | S_IFLNK => {}
            S_IFDIR => {
                return Err(VolumeError::Format(
                    "a directory has no contents to read".into(),
                ));
            }
            _ => return Err(VolumeError::Format("not a regular file".into())),
        }
        let size = ino.size;
        let root = self.tree_root(id.tree)?;
        let mut pos = 0u64;
        let mut from = 0u64;
        'all: loop {
            let (batch, more) = self.extents(root, id.ino, from)?;
            for e in &batch {
                if pos >= size {
                    break 'all;
                }
                self.write_extent(e, &mut pos, size, out)?;
            }
            match (more, batch.last()) {
                (true, Some(last)) => from = last.off + 1,
                _ => break,
            }
        }
        write_zeros(out, size.saturating_sub(pos))?; // díra na konci (a soubor zkrácený na menší velikost než extenty)
        Ok(size)
    }

    /// Dávka položek `EXTENT_DATA` souboru od posunu `from`; druhý údaj říká, jestli je další dávka.
    fn extents(
        &mut self,
        root: u64,
        ino: u64,
        from: u64,
    ) -> Result<(Vec<Extent>, bool), VolumeError> {
        let mut list: Vec<Extent> = Vec::new();
        let mut more = false;
        let mut bad: Option<VolumeError> = None;
        self.visit(
            root,
            Key(ino, EXTENT_DATA, from),
            Key(ino, EXTENT_DATA, u64::MAX),
            &mut |k, d| {
                if list.len() >= EXTENT_BATCH {
                    more = true;
                    return Flow::Stop;
                }
                if d.len() < FileExtentItem::HEADER_SIZE {
                    bad = Some(corrupt("a file extent item is too short"));
                    return Flow::Stop;
                }
                if d[17] != 0 || d[18] != 0 || d[19] != 0 {
                    bad = Some(VolumeError::Unsupported(
                        "encrypted or specially encoded file extents are not supported".into(),
                    ));
                    return Flow::Stop;
                }
                let Some(item) = FileExtentItem::parse(d) else {
                    bad = Some(corrupt("an unreadable file extent item"));
                    return Flow::Stop;
                };
                let inline = match item.body {
                    FileExtentBody::Inline { .. } => d[FileExtentItem::HEADER_SIZE..].to_vec(),
                    FileExtentBody::Regular { .. } => Vec::new(),
                };
                list.push(Extent {
                    off: k.2,
                    item,
                    inline,
                });
                Flow::Continue
            },
        )?;
        if let Some(e) = bad {
            return Err(e);
        }
        Ok((list, more))
    }

    fn write_extent(
        &mut self,
        e: &Extent,
        pos: &mut u64,
        size: u64,
        out: &mut dyn Write,
    ) -> Result<(), VolumeError> {
        let comp = e.item.compression;
        // Délka logických bajtů, které extent pokrývá.
        let num = match (&e.item.body, e.item.extent_type) {
            (FileExtentBody::Inline { inline_size }, _) => {
                if comp == CompressionType::None {
                    *inline_size as u64
                } else {
                    e.item.ram_bytes
                }
            }
            (FileExtentBody::Regular { num_bytes, .. }, _) => *num_bytes,
        };
        // díra před extentem
        if e.off > *pos {
            let gap = e.off.min(size) - *pos;
            write_zeros(out, gap)?;
            *pos += gap;
            if *pos >= size {
                return Ok(());
            }
        }
        // překryv s tím, co už bylo vypsáno (nemá se stát; kdyby ano, pokračuje se tam, kde se skončilo)
        let skip = pos.saturating_sub(e.off);
        if skip >= num {
            return Ok(());
        }
        let take = (num - skip).min(size - *pos);
        match (&e.item.body, e.item.extent_type) {
            (FileExtentBody::Inline { .. }, _) => {
                let data = if comp == CompressionType::None {
                    std::borrow::Cow::Borrowed(&e.inline[..])
                } else {
                    std::borrow::Cow::Owned(self.decompress(comp, &e.inline, e.item.ram_bytes)?)
                };
                let s = skip as usize;
                let t = take as usize;
                let part = data
                    .get(s..s + t)
                    .ok_or_else(|| corrupt("an inline extent is shorter than the file says"))?;
                out.write_all(part)?;
            }
            (
                FileExtentBody::Regular {
                    disk_bytenr,
                    disk_num_bytes,
                    offset,
                    ..
                },
                ftype,
            ) => {
                if ftype == FileExtentType::Prealloc || *disk_bytenr == 0 {
                    write_zeros(out, take)?; // díra zapsaná jako extent nebo preallokovaný (zatím nezapsaný) extent
                } else if comp == CompressionType::None {
                    let mut at = disk_bytenr + offset + skip;
                    let mut left = take;
                    while left > 0 {
                        let n = left.min(READ_PIECE);
                        let buf = self.reader.read_data(at, n as usize)?;
                        out.write_all(&buf)?;
                        at += n;
                        left -= n;
                    }
                } else {
                    let (db, dn) = (*disk_bytenr, *disk_num_bytes);
                    let hit = matches!(&self.last_extent, Some((a, l, _)) if *a == db && *l == dn);
                    if !hit {
                        let raw = self.reader.read_data(db, dn as usize)?;
                        let data = self.decompress(comp, &raw, e.item.ram_bytes)?;
                        self.last_extent = Some((db, dn, data));
                    }
                    let data = &self
                        .last_extent
                        .as_ref()
                        .expect("the extent was just stored")
                        .2;
                    let s = (offset + skip) as usize;
                    let t = take as usize;
                    let part = data.get(s..s + t).ok_or_else(|| {
                        corrupt("a compressed extent is shorter than a file range that uses it")
                    })?;
                    out.write_all(part)?;
                }
            }
        }
        *pos += take;
        Ok(())
    }

    fn decompress(
        &self,
        kind: CompressionType,
        data: &[u8],
        ram: u64,
    ) -> Result<Vec<u8>, VolumeError> {
        let ram = usize::try_from(ram).map_err(|_| corrupt("an extent is too big"))?;
        if ram > 1 << 30 {
            return Err(corrupt(
                "an extent claims an unreasonable size (damaged file system)",
            ));
        }
        let mut out = Vec::with_capacity(ram);
        match kind {
            CompressionType::None => out.extend_from_slice(data),
            CompressionType::Zlib => {
                flate2::read::ZlibDecoder::new(data)
                    .take(ram as u64)
                    .read_to_end(&mut out)
                    .map_err(|e| corrupt(format!("zlib: {e}")))?;
            }
            CompressionType::Zstd => {
                let dec = ruzstd::decoding::StreamingDecoder::new(data)
                    .map_err(|e| corrupt(format!("zstd: {e}")))?;
                let mut dec = dec.take(ram as u64);
                dec.read_to_end(&mut out)
                    .map_err(|e| corrupt(format!("zstd: {e}")))?;
            }
            CompressionType::Lzo => {
                out = lzo::decompress_btrfs(data, self.sectorsize, ram)
                    .map_err(|e| corrupt(e.to_string()))?;
            }
            CompressionType::Unknown(v) => {
                return Err(VolumeError::Unsupported(format!(
                    "unknown extent compression type {v}"
                )));
            }
        }
        // Stejně jako jádro: co algoritmus nedodal, jsou nuly.
        if out.len() < ram {
            out.resize(ram, 0);
        }
        Ok(out)
    }
}

struct Extent {
    /// Posun v souboru.
    off: u64,
    item: FileExtentItem,
    /// Data vloženého (inline) extentu tak, jak jsou uložená (případně komprimovaná).
    inline: Vec<u8>,
}

fn write_zeros(out: &mut dyn Write, mut n: u64) -> Result<(), VolumeError> {
    const ZEROS: [u8; 65536] = [0; 65536];
    while n > 0 {
        let k = n.min(ZEROS.len() as u64) as usize;
        out.write_all(&ZEROS[..k])?;
        n -= k as u64;
    }
    Ok(())
}
