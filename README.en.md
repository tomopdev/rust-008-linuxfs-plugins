# linuxfs-plugins

*[Česky](README.cs.md)*

**Browse and extract Linux file systems (btrfs, ext2/3/4, SquashFS) in Total Commander and Altap/Open Salamander. Read-only, written in Rust.**

![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)
![rust](https://img.shields.io/badge/Rust-edition%202024-orange)
![platform](https://img.shields.io/badge/platform-Windows%20x64-lightgrey)
![mode](https://img.shields.io/badge/mode-read--only-green)

The plugins show a Linux disk image, partition or whole physical disk as an ordinary archive: browse folders, view files (F3) and
extract them (F5). Nothing is ever written. The project follows the architecture of macfs-plugins; more file systems will be added step by step.

* `linuxfs_wcx.wcx64` – **WCX** plugin for Total Commander 11.x (64 bit)
* `linuxfs.spl` – plugin for **Altap Salamander 4.0 / Open Salamander 5.0** (64 bit)

Both plugins share the same core (`linuxfs-core`), so they do the same.

> **Status:** developed and tested on Linux against **images made by real Linux tools** (btrfs: `mkfs.btrfs` + mount, contents compared with what the
> kernel reports; ext2/3/4: `mke2fs -d` from a source directory, checked with `e2fsck`; SquashFS: `mksquashfs` from a source directory) and against the plugin ABIs through mock hosts. Manual tests in real
> Total Commander and Salamander passed for btrfs (several partitions on one disk, a partition with 8 subvolumes). **ext2/3/4**, **SquashFS** support and the new
> **error reporting** are verified by automated tests only; the Windows (MSVC) build has to be done on your side – see [Limitations](#limitations).

## What is implemented

| Area | Format | Status | Notes |
|---|---|---|---|
| File system | **btrfs** | ✅ read-only | own layer over the vendored `btrfs-disk` 0.13.0 (on-disk structs): B-trees, extents, subvolumes |
| | **ext2, ext3, ext4** | ✅ read-only | layer over the vendored `ext4-view` 1.0.0 (extents and indirect blocks, HTree, 64bit, `metadata_csum`); the kind is told from feature flags like the kernel does |
| | **SquashFS** 4.0 | ✅ read-only | crate `rust-fs-squashfs` 0.4.0 (pure Rust, MIT) through an own adapter over `DiskSource`; **gzip, xz, lzo, lz4, zstd** compression (and legacy lzma), fragments, sparse files; items are addressed by inode reference |
| Compression (btrfs) | zlib, **zstd**, **LZO** | ✅ | zstd via `ruzstd`, zlib via `flate2`, LZO with an own safe decoder |
| Subvolumes (btrfs) | subvolumes, nested subvolumes, snapshots | ✅ | the top level shows all subvolumes as folders; one can be selected (`subvol=`, `subvolid=`) |
| Journal (ext3/4) | not cleanly unmounted volume | ✅ in memory only | the journal is **never replayed on disk**, only mapped in memory; you see the state a mount would give after recovery |
| Files | regular, sparse, prealloc, inline, reflinks, hard links | ✅ | sizes above 4 GiB verified |
| | symbolic links | ✅ | shown as a small file containing the target |
| | fifos, sockets, devices | hidden | meaningless on Windows |
| Partitions | bare volume, **GPT** (512 and 4096 byte sectors), **MBR** (primary and logical) | ✅ | found automatically; several = folders `Partition N - name`; btrfs, ext and SquashFS can sit side by side on one disk |
| Source | image, physical disk / partition | ✅ | disks through a text file `.linuxfsdev` |

Each volume has a virtual description file in its root: `!BTRFS-INFO.txt` (label, UUID, size, subvolume list, default subvolume) or
`!EXT2-INFO.txt` / `!EXT3-INFO.txt` / `!EXT4-INFO.txt` (label, UUID, sizes, features, state, journal information) or
`!SQUASHFS-INFO.txt` (compression, block size, inode count, creation time, image flags).

Linux file names are arbitrary bytes; the plugins map them to Windows-safe names (illegal characters and reserved names → `_`, invalid UTF-8
replaced, case-insensitive collisions get ` (2)`); extraction always uses the original entry, not the mapped name.

## Errors and partial errors

**General principle of both plugins: an error in one file, directory or partition never stops the rest of the work.** What cannot be read is recorded, the rest is
shown / extracted, and at the end a summary is shown in a window while the **full list is saved to a text file** in `%TEMP%` (UTF-8 with BOM; overwritten by the next operation of the same kind):

| File | Created when | Contains |
|---|---|---|
| `linuxfs-errors-list.txt` | listing | directories that could not be listed; entries with an unreadable inode; partitions that failed to open (skipped); in Salamander, items the host refused (paths of about 260 characters or more, unacceptable names) |
| `linuxfs-errors-unpack.txt` | extracting / viewing | files that could not be read or written, and why |

* **Salamander** limits paths to 260 characters; such items are skipped (with their contents) and listed in the window and in the file. When extraction fails it still asks
  *skip / skip all with errors / cancel*; every error is additionally recorded and summarised at the end.
* **Total Commander** has no such limit. Listing problems are reported by a window when an archive opened for listing is closed, extraction problems by a window after extraction;
  a failing file returns an error to TC (which offers skip / abort) and an unfinished target file is always deleted.
* A damaged volume is read as far as possible: on **ext** a damaged directory block is skipped and the following blocks are still listed, and files in intact blocks stay readable
  (local patch of `ext4-view`, see `vendor/ext4-view/PATCHES.md`); on **btrfs** an entry with a missing inode is left out and the directory stays readable.

## Install

**Total Commander** – open `dist\linuxfs_wcx.zip` (it contains `pluginst.inf`) in TC and confirm. Associate extensions (`btrfs`, `ext4`, `squashfs`, `img`, `linuxfsdev`)
in *Configuration → Options → Packer*. The plugin detects the format by content, so any file can be opened manually (**Ctrl+PgDn**).

**Salamander** – copy `linuxfs.spl` to `plugins\linuxfs\` and add it in *Plugins → Plugins Manager*. Registered extensions: `btrfs;ext2;ext3;ext4;squashfs;sqfs;sfs;img;raw;dd;linuxfsdev`.

## Usage

* **Image or volume**: double-click / Ctrl+PgDn on `.img`, `.raw`, `.btrfs`, `.ext4`, `.squashfs` / `.sqfs`… With several partitions each one gets its own folder.
* **Real disk or partition**: a text file `linux.linuxfsdev`

  ```ini
  device = \\.\PhysicalDrive2
  partition = 2          ; optional: ORDER among the volumes found (1 = first), not the disk partition number
  # offset = 209735680   ; or an exact byte offset
  # length = ...         ; optional length
  subvol = @home         ; optional, btrfs only: show only this subvolume ("/" = top level, "default" = default)
  # subvolid = 257       ; or by id
  ```

  Reading a raw disk needs administrator rights. The disk is **never** written. Reading a mounted, actively written volume may give inconsistent results;
  an ext3/4 volume with an unreplayed journal (unclean shutdown, hibernation) is shown as it would look after recovery.
* On btrfs, without `subvol=` the whole volume (top level, i.e. `@`, `@home`, snapshots as folders) is shown.

## Build

Requirements: Rust ≥ 1.97, `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (C++ workload). The first build downloads crates (`crc`, `bitflags`, … for ext), so it needs internet.

```powershell
.\build.ps1                  # tests of the whole workspace + both plugins -> dist\
.\build.ps1 -Plugin wcx      # Total Commander only
.\build.ps1 -Plugin spl      # Salamander only
.\build.ps1 -Native          # -C target-cpu=native
```

`cargo test --workspace --release` also runs on Linux/macOS. Fixtures can be regenerated with `tools/make-btrfs-fixtures.sh` (Linux, root, btrfs-progs), `tools/make-ext-fixtures.sh` (Linux, **no root needed**, e2fsprogs) and
`tools/make-squashfs-fixtures.sh` (Linux, **no root needed**, squashfs-tools).

## Layout

`crates/linuxfs-core` (source, detect, btrfs, ext, squashfs, lzo, volume, report), `crates/linuxfs-wcx`, `crates/linuxfs-spl`, `vendor/btrfs-disk` and `vendor/ext4-view`
(patched upstream crates, see `PATCHES.md`), `tools/`, `build.ps1`.

## Tests

The ext images (`ext2`, `ext3`, `ext4`) were made by `mke2fs -d` from a source directory (1 KiB / 4 KiB blocks, journal, extents, `metadata_csum`, 3000 files in a directory, deep trees,
fast and slow symlinks, invalid UTF-8 in a name, a sparse file with holes in indirect blocks); the manifest comes from the source and `e2fsck -fn` verified the image.
`ext4-damaged` has a zeroed inode, a zeroed directory and one zeroed block of a large directory, and checks that the rest is read and the errors are reported.
The SquashFS images (`squashfs-gzip`, `-xz`, `-zstd`, `-lz4`, `-lzo`) were made by `mksquashfs` from one source directory (different compression and block size from 4 KiB to 1 MiB, with and without fragments and export table;
1500 files in a directory spanning several metadata blocks, 200 small files in fragments, a sparse file, a deep tree, symlinks, invalid UTF-8 in a name); the shared `squashfs.manifest` comes from the source.
`squashfs-damaged` has a destroyed compressed data block of one file and the first directory metadata block, and checks that the rest is read and the errors are reported.
The plugin tests run against mock hosts and also check the windows and the text files with errors.

## Limitations

* The Windows (MSVC) build could not be verified here; the Windows-only parts (the message window in the TC plugin) were therefore not compiled. Manual tests in real TC / Salamander
  passed for btrfs; ext and the error reporting have not been tried there yet.
* **Read-only**; write support is not implemented (for ext4 see [below](#writing-to-ext4-is-it-possible-and-what-would-it-cost)).
* **btrfs multi-device** (RAID0/1/10/5/6 across disks): only the given device is read. btrfs data checksums are **not verified**. Refused: zoned, raid-stripe-tree, remap-tree, extent-tree-v2;
  fscrypt-encrypted extents are not read.
* **ext:** refused features (the volume does not open, with a message): compression, `meta_bg`, MMP, `ea_inode`, `dirdata`, `large_dir`, **`inline_data`**, a separate journal device,
  and ext2 **without `filetype`** (very old volumes). fscrypt-encrypted items are not read. Metadata checksums (`metadata_csum`) are verified, data is not.
  The journal is only mapped in memory (no recovery on disk).
* **SquashFS:** version 4.0 only (older 1.x–3.x are not recognised); gzip, xz, lzo, lz4, zstd and legacy lzma (best effort only). The image has no data checksums, so damage is noticed only where decompression
  or a structure check fails; a truncated image does not open (with a message). `rust-fs-squashfs` is a young crate (0.4.0, one author); correctness is verified on `mksquashfs` images only (see Tests).
  A directory listing is decompressed as a whole, so a directory with a damaged metadata block is not listed at all (it is reported as an unreadable directory).
* xattrs, ACLs and Linux permissions are not shown; times are mtime converted to UTC. Physical drives and MBR extended partitions were tested on images only.
* `ext4-view` is an upstream 1.0 crate with local patches; two of them fix real problems (sparse files on ext2/3, listing aborted by a damaged block). They should be reported upstream;
  once merged, `vendor/ext4-view` can be dropped.

## Writing to ext4: is it possible, and what would it cost

**Status: write support is not implemented, and this version writes nothing.** Technically, though, **it could be added** if it were ever needed. Assessment (from the crates'
documentation and sources, October 2026; none of it was tried in practice):

**Available crates with write support**

| Crate | License | Verdict | Notes |
|---|---|---|---|
| `rust-fs-ext4` 0.8.0 (formerly `am-fs-ext4`) | MIT | **most promising** | pure-Rust ext2/3/4, read and write **through the JBD2 journal** (four-fence protocol with flushes between steps, crash-injection tests), journal replay on mount, `metadata_csum`, `inline_data`, xattrs, ACLs; its `rust-fs-core` block layer (`BlockDevice`, `CallbackDevice`) can be wrapped around our `DiskSource`. **Untried**, per its README only: very young (under this name since 2026-10-06, a few hundred downloads, one author), no extent-tree mutation at depth ≥ 2, directories beyond ~340 blocks error out, the indirect-block "file replace" on ext2/ext3 without a journal is not journaled; it is unclear whether it can be mounted truly read-only without replaying the journal on disk (that would break the "nothing is written" guarantee) |
| `ext4plus` 0.1.0-rc.2 | MIT OR Apache-2.0 | weaker candidate | hard fork of `ext4-view` (same architecture as now), read and write (`std::fs`-like API), `no_std`. The author says: *beta, known bugs, use a ramdisk when writing*; it **does not write the journal** (recommends disabling journaling), no `inline_data`, `bigalloc`, external xattr blocks |
| `ext4_rs` 1.3.3 | MIT | less suitable | write supported (create, write, `mkdir`, `unlink`, truncate), ext4 only; small user base, earlier versions yanked, two known bugs fixed |
| `ext4-lwext4` 0.1.2 | MIT OR Apache-2.0 (wrapper) | **unsuitable** | wraps the C library *lwext4*, parts of which (`ext4_extent.c`, `ext4_xattr.c`) are **GPL-2.0**; per its README those files are left out by default, and with the `gpl-*` features the result is GPL-2.0 – ext4 without extents is practically unusable for today's volumes. It also needs a C compiler |

**Risks (most serious first)**

1. **Damage to user data is irreversible and asymmetric.** A read-only plugin cannot harm anything; a writing plugin with a single bug can silently corrupt a whole Linux system on a disk (often the only copy).
   `ext4plus` is pre-0.1.0 with admitted bugs, so it would be a source of risk by itself.
2. **The journal.** Writing outside the journal (what `ext4plus` does; `rust-fs-ext4` journals its writes, which reduces this risk considerably, but untested) is not crash-safe: a Windows crash, a USB unplug or an abort halfway leaves the volume inconsistent (a directory pointing to an
   unallocated block, etc.). Moreover a volume with an **unreplayed journal** (`needs_recovery`) must not be written at all – the old journal would be replayed **over** the newly written data at the next mount in Linux.
   The fix would be to replay the journal on disk first (a new, risky feature `ext4-view` does not have) or to refuse such volumes.
3. **Mounted / hibernated / shared volume.** Dual boot with hibernation or fast startup, or a volume mounted elsewhere (network, VM), would be written under another kernel's feet. Everything not cleanly unmounted
   would have to be refused (`s_state`, `needs_recovery`, MMP, recent write time) – relying on those flags not lying.
4. **Raw disk access on Windows.** Writing to `\\.\PhysicalDriveN` needs administrator rights, volume locking / dismounting, sector alignment, cache flushing (`FlushFileBuffers`) and a write-back cache
   with read-modify-write (today there is only a 128 KiB read cache). Windows can also touch an "unrecognised" disk on its own.
5. **Feature coverage.** A volume with a feature the crate does not know (`inline_data`, `bigalloc`, quotas, `meta_bg`, MMP…) must not be written at all – a list of "safe" volumes and refusal of the rest is needed,
   otherwise silent corruption is possible. The same goes for encrypted (fscrypt) and casefold directories.
6. **Metadata consistency.** A write must keep the bitmaps, free block / inode counts (also in superblock backups and group descriptors), `metadata_csum` checksums, times and `i_blocks`,
   the orphan list and sparse-super backups correct. A mistake shows only in `e2fsck` or at the next Linux boot.
7. **Archive API semantics.** WCX (`PackFiles`, `DeleteFiles`) and Salamander (`PackToArchive`, `DeleteFromArchive`) have their own ideas of aborting, overwriting and renaming; on ext4 rename and overwrite are not atomic
   without a journal. New files would get an owner / mode / SELinux labels Windows does not know (on an SELinux system a `restorecon` may be needed).

**If it were ever done, the recommended approach:** (1) **images and non-system partitions only** at first, never a live disk without an explicit `write = yes` in `.linuxfsdev`; (2) refuse anything that is not clean
(no `needs_recovery`, no unknown features, no MMP), possibly offering a copy first; (3) start with adding and overwriting files and `mkdir`, deleting and renaming later; (4) run `e2fsck -fn` after every operation in the tests on
`mke2fs`-made images (as is done for reading today) and add fuzzing / interruption in the middle of a write; (5) consider whether **`wsl --mount`** is enough instead (WSL2 mounts a physical ext4/btrfs disk with the native
Linux driver, including write, journal and recovery) – for writing that is safer than any user-space crate.
Rough, unverified estimate: minimal writing of new files and folders on a clean ext4 without a journal, with tests, ≈ 1–2 weeks; with safe journal replay and crash resistance several times more.

## Roadmap (rough, unverified estimates)

ext: `inline_data`, `meta_bg`, MMP, `bigalloc` (days to weeks each, in `vendor/ext4-view` or upstream), XFS (1–2 weeks), F2FS (≈1 week), LVM2 (4–7 days), mdraid (4–6 days), LUKS1/2 (1–2 weeks),
btrfs multi-device (3–5 days), btrfs RAID5/6 (≈1 week), btrfs checksum verification (1–2 days), btrfs write support (weeks; risky, not recommended without extensive tests), ext4 write support (1–2 weeks minimum, risky, see above),
ZFS/bcachefs (out of scope for now). New file systems are added as a module in `linuxfs-core`; the plugins need no change.

## License

**MIT or Apache-2.0, at your option** ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)); it used to be GPL-3.0-or-later for continuity with macfs-plugins. No dependency requires GPL,
see [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md). FrankenFS was not used because of its license rider excluding certain parties; `ext4-lwext4` because of the GPL parts of lwext4.

## Acknowledgements

The crates `btrfs-disk` (rustutils/btrfsutils), `ext4-view` (Nicholas Bishop, Google), `rust-fs-squashfs` (antimatter-studios), `ruzstd`, `flate2`, the btrfs and ext4 documentation (kernel.org) and the macfs-plugins project the plugin structure comes from.
