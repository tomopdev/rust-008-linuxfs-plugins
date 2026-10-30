//! Společné jádro pluginů `linuxfs-plugins`.
//!
//! * [`source`] – zdroj bajtů (soubor s obrazem nebo raw zařízení) s cache zarovnanou na sektory,
//! * [`detect`] – hledání svazků uvnitř zdroje (holý svazek, GPT, MBR včetně logických oddílů, soubory `.linuxfsdev`),
//! * [`btrfs`] – btrfs jen pro čtení nad crate `btrfs-disk` (podsvazky, komprese zlib / LZO / zstd),
//! * [`ext`] – ext2 / ext3 / ext4 jen pro čtení nad crate `ext4-view`,
//! * [`squashfs`] – SquashFS jen pro čtení nad crate `rust-fs-squashfs` (komprese gzip / xz / lzo / lz4 / zstd),
//! * [`report`] – sběr chyb a upozornění při výpisu a rozbalování a jejich zápis do textového souboru,
//! * [`lzo`] – dekodér LZO1X a rámování LZO, které používá btrfs,
//! * [`volume`] – jednotný pohled na jeden svazek, adresace položek číslem,
//! * [`util`] – převody jmen (očista pro Windows), časů a cest.

pub mod btrfs;
pub mod detect;
pub mod ext;
pub mod lzo;
pub mod report;
pub mod source;
pub mod squashfs;
pub mod util;
pub mod volume;
