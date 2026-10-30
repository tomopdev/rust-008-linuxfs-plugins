# linuxfs-plugins

*[Česky](README.cs.md)*

**Browse and extract Linux file systems (btrfs so far) in Total Commander and Altap/Open Salamander. Read-only, written in Rust.**

![license](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)
![rust](https://img.shields.io/badge/Rust-edition%202024-orange)
![platform](https://img.shields.io/badge/platform-Windows%20x64-lightgrey)
![mode](https://img.shields.io/badge/mode-read--only-green)

The plugins show a Linux disk image, partition or whole physical disk as an ordinary archive: browse folders, view files (F3) and
extract them (F5). Nothing is ever written. The project follows the architecture of macfs-plugins; more file systems will be added step by step.

* `linuxfs_wcx.wcx64` – **WCX** plugin for Total Commander 11.x (64 bit)
* `linuxfs.spl` – plugin for **Altap Salamander 4.0 / Open Salamander 5.0** (64 bit)

Both plugins share the same core (`linuxfs-core`), so they do the same.

> **Status:** developed and tested on Linux against **btrfs images made by a real Linux kernel** (`mkfs.btrfs` + mount; the tests compare
> contents with what the kernel reports) and against the plugin ABIs through mock hosts. The Windows (MSVC) build and a run inside a real
> Total Commander / Salamander **were not verified** where it was developed – see [Limitations](#limitations).

## What is implemented

| Area | Format | Status | Notes |
|---|---|---|---|
| File system | **btrfs** | ✅ read-only | own layer over the vendored `btrfs-disk` 0.13.0 (on-disk structs): B-trees, extents, subvolumes |
| Compression | zlib, **zstd**, **LZO** | ✅ | zstd via `ruzstd`, zlib via `flate2`, LZO with an own safe decoder |
| Subvolumes | subvolumes, nested subvolumes, snapshots | ✅ | the top level shows all subvolumes as folders; one can be selected (`subvol=`, `subvolid=`) |
| Files | regular, sparse, prealloc, inline, reflinks, hard links | ✅ | sizes above 4 GiB verified |
| | symbolic links | ✅ | shown as a small file containing the target |
| | fifos, sockets, devices | hidden | meaningless on Windows |
| Partitions | bare volume, **GPT** (512 and 4096 byte sectors), **MBR** (primary and logical) | ✅ | found automatically; several = folders `Partition N - name` |
| Source | image, physical disk / partition | ✅ | disks through a text file `.linuxfsdev` |

Each volume has a virtual `!BTRFS-INFO.txt` in its root (label, UUID, size, subvolume list, default subvolume).
btrfs file names are arbitrary bytes; the plugins map them to Windows-safe names (illegal characters and reserved names → `_`, invalid UTF-8
replaced, case-insensitive collisions get ` (2)`); extraction always uses the original entry, not the mapped name.

## Install

**Total Commander** – open `dist\linuxfs_wcx.zip` (it contains `pluginst.inf`) in TC and confirm. Associate extensions (`btrfs`, `img`, `linuxfsdev`)
in *Configuration → Options → Packer*. The plugin detects the format by content, so any file can be opened manually (**Ctrl+PgDn**).

**Salamander** – copy `linuxfs.spl` to `plugins\linuxfs\` and add it in *Plugins → Plugins Manager*. Registered extensions: `btrfs;img;raw;dd;linuxfsdev`.

## Usage

* **Image or volume**: double-click / Ctrl+PgDn on `.img`, `.raw`, `.btrfs`… With several partitions each one gets its own folder.
* **Real disk or partition**: a text file `linux.linuxfsdev`

  ```ini
  device = \\.\PhysicalDrive2
  partition = 2          ; optional: ORDER among the btrfs volumes found (1 = first), not the disk partition number
  # offset = 209735680   ; or an exact byte offset
  # length = ...         ; optional length
  subvol = @home         ; optional: show only this subvolume ("/" = top level, "default" = default)
  # subvolid = 257       ; or by id
  ```

  Reading a raw disk needs administrator rights. The disk is **never** written. Reading a mounted, actively written volume may give inconsistent results.
* Without `subvol=` the whole volume (top level, i.e. `@`, `@home`, snapshots as folders) is shown.

## Build

Requirements: Rust ≥ 1.97, `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (C++ workload).

```powershell
.\build.ps1                  # tests of the whole workspace + both plugins -> dist\
.\build.ps1 -Plugin wcx      # Total Commander only
.\build.ps1 -Plugin spl      # Salamander only
.\build.ps1 -Native          # -C target-cpu=native
```

`cargo test --workspace --release` also runs on Linux/macOS. Fixtures can be regenerated with `tools/make-btrfs-fixtures.sh` (Linux, root, btrfs-progs).

## Layout

`crates/linuxfs-core` (source, detect, btrfs, lzo, volume), `crates/linuxfs-wcx`, `crates/linuxfs-spl`, `vendor/btrfs-disk` (patched, see `PATCHES.md`), `tools/`, `build.ps1`.

## Limitations

* Windows build and real TC / Salamander runs not verified. **Read-only**; write support is not implemented.
* **Multi-device** btrfs (RAID0/1/10/5/6 across disks): only the given device is read; data on other devices is unreadable.
* Data checksums are **not verified**. Unsupported (volume refused): zoned, raid-stripe-tree, remap-tree, extent-tree-v2; fscrypt-encrypted extents are not read.
* xattrs, ACLs and Linux permissions are not shown; times are mtime converted to UTC. Physical drives and MBR extended partitions were tested on images only.

## Roadmap (rough, unverified estimates)

ext2/3/4 (3–5 days), XFS (1–2 weeks), F2FS (≈1 week), LVM2 (4–7 days), mdraid (4–6 days), LUKS1/2 (1–2 weeks), btrfs multi-device (3–5 days),
RAID5/6 (≈1 week), checksum verification (1–2 days), btrfs write support (weeks; risky, not recommended without extensive tests), ZFS/bcachefs (out of scope for now).
New file systems are added as a module in `linuxfs-core`; the plugins need no change.

## License

GPL-3.0-or-later ([LICENSE](LICENSE)); no dependency requires GPL, see [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
FrankenFS was not used because of its license rider excluding certain parties.
