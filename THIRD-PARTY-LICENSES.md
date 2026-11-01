# Licence použitých knihoven

Seznam je vygenerován z `cargo metadata` (včetně přechodných závislostí, vendorovaného `btrfs-disk` a závislostí jen pro jiné platformy / testy).
Projekt je šířen pod **MIT OR Apache-2.0** (viz `LICENSE-MIT` a `LICENSE-APACHE`; dřív GPL-3.0-or-later kvůli kontinuitě s projektem macfs-plugins). **Žádná použitá knihovna GPL ani jinou copyleftovou licenci nevyžaduje** – všechny jsou MIT / Apache-2.0 / BSD / Zlib / BSL-1.0 apod. (u `r-efi` je LGPL jen jedna z alternativ, používá se MIT / Apache-2.0).

Záměrně nepoužito: **FrankenFS** (a jeho crate) – licence obsahuje dodatek (rider), který zakazuje použití určitým subjektům včetně Anthropic, takže ho
nelze použít v projektu vytvářeném s Claude. Použité crates `btrfs-disk` a `ext4-view` (oba MIT OR Apache-2.0) jsou vendorovány s lokálními
úpravami popsanými ve `vendor/btrfs-disk/PATCHES.md` a `vendor/ext4-view/PATCHES.md`.

Pro SquashFS se používá crate **`rust-fs-squashfs`** 0.4.0 (MIT, čistý Rust, nevendorovaný) nad blokovou vrstvou `rust-fs-core` 0.3 (MIT); jeho dekodéry jsou `lzma-rs` (xz / lzma), `lz4_flex`, `rust-lzo1x` a `ruzstd` 0.8 (vedle `ruzstd` 0.9, který používá btrfs) – všechny MIT.

Pro XFS se používá crate **`rust-fs-xfs`** 0.14.2 (MIT, čistý Rust, nevendorovaný, stejný autor a stejná blokovou vrstva `rust-fs-core` jako `rust-fs-squashfs`); přibírá jen `crc32c` (kontrolní součty metadat v5). Crate umí i zápis a C ABI, projekt používá jen čtecí část.

Pro EROFS se používá crate **`rust-fs-erofs`** 0.5.0 (MIT, čistý Rust, nevendorovaný, stejný autor a stejná blokovou vrstva `rust-fs-core` jako předchozí); přibírá `crc32c`, `flate2`, `lru`, `lz4_flex`, `lzma-rs` a `ruzstd` 0.8 (všechny už v projektu). Crate umí i zápis (mkfs) a C ABI, projekt používá jen čtecí část.

Záměrně nepoužito: **ext4-lwext4 / ext4-lwext4-sys** (obalují C knihovnu lwext4, jejíž části jsou GPL-2.0 – s příznaky `gpl-*` by se
výsledek stal GPL) a crates bez uvedené licence.

| Crate | Verze | Licence | Zdroj |
|---|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | https://github.com/oyvindln/adler2 |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 | https://github.com/zakarumych/allocator-api2 |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags |
| blake2 | 0.10.6 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| btrfs-disk | 0.13.0 | MIT OR Apache-2.0 | https://github.com/rustutils/btrfsutils |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 | https://github.com/fitzgen/bumpalo |
| byteorder | 1.5.0 | Unlicense OR MIT | https://github.com/BurntSushi/byteorder |
| bytes | 1.12.1 | MIT | https://github.com/tokio-rs/bytes |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/cfg-if |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| crc | 3.4.0 | MIT OR Apache-2.0 | https://github.com/mrhooray/crc-rs.git |
| crc-catalog | 2.5.0 | MIT OR Apache-2.0 | https://github.com/akhilles/crc-catalog.git |
| crc32c | 0.6.8 | Apache-2.0/MIT | https://github.com/zowens/crc32c |
| crc32fast | 1.5.3 | MIT OR Apache-2.0 | https://github.com/srijs/rust-crc32fast |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| digest | 0.10.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| equivalent | 1.0.3 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/equivalent |
| ext4-view | 1.0.0 | MIT OR Apache-2.0 | https://github.com/nicholasbishop/ext4-view-rs |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | https://github.com/rust-lang/flate2-rs |
| foldhash | 0.1.5 | Zlib | https://github.com/orlp/foldhash |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-task | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-util | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| generic-array | 0.14.7 | MIT | https://github.com/fizyk20/generic-array.git |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| hashbrown | 0.15.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| js-sys | 0.3.106 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys |
| libc | 0.2.191 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| lru | 0.12.5 | MIT | https://github.com/jeromefroe/lru-rs.git |
| lz4_flex | 0.11.6 | MIT | https://github.com/pseitz/lz4_flex |
| lzma-rs | 0.3.0 | MIT | https://github.com/gendx/lzma-rs |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | https://github.com/matklad/once_cell |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project-lite |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| quote | 1.0.47 | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | https://github.com/r-efi/r-efi |
| rust-fs-core | 0.3.17 | MIT | https://github.com/antimatter-studios/rust-fs-core |
| rust-fs-erofs | 0.5.0 | MIT | https://github.com/antimatter-studios/rust-fs-erofs |
| rust-fs-squashfs | 0.4.0 | MIT | https://github.com/antimatter-studios/rust-fs-squashfs |
| rust-fs-xfs | 0.14.2 | MIT | https://github.com/antimatter-studios/rust-fs-xfs |
| rust-lzo1x | 0.4.0 | MIT | https://github.com/antimatter-studios/rust-lzo1x |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 | https://github.com/djc/rustc-version-rs |
| rustversion | 1.0.23 | MIT OR Apache-2.0 | https://github.com/dtolnay/rustversion |
| ruzstd | 0.8.3 | MIT | https://github.com/KillingSpark/zstd-rs |
| ruzstd | 0.9.1 | MIT | https://github.com/KillingSpark/zstd-rs |
| semver | 1.0.28 | MIT OR Apache-2.0 | https://github.com/dtolnay/semver |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| simd-adler32 | 0.3.10 | MIT | https://github.com/mcountryman/simd-adler32 |
| slab | 0.4.12 | MIT | https://github.com/tokio-rs/slab |
| subtle | 2.6.1 | BSD-3-Clause | https://github.com/dalek-cryptography/subtle |
| syn | 3.0.9 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| twox-hash | 2.1.5 | MIT | https://github.com/shepmaster/twox-hash |
| typenum | 1.20.1 | MIT OR Apache-2.0 | https://github.com/paholg/typenum |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| uuid | 1.29.0 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| version_check | 0.9.5 | MIT/Apache-2.0 | https://github.com/SergioBenitez/version_check |
| wasm-bindgen | 0.2.129 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen |
| wasm-bindgen-macro | 0.2.129 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro |
| wasm-bindgen-macro-support | 0.2.129 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support |
| wasm-bindgen-shared | 0.2.129 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| xxhash-rust | 0.8.19 | BSL-1.0 | https://github.com/DoumanAsh/xxhash-rust |
| zlib-rs | 0.6.8 | Zlib | https://github.com/trifectatechfoundation/zlib-rs |
