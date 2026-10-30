# Lokální úpravy crate `btrfs-disk` 0.13.0

Upstream: <https://github.com/rustutils/btrfsutils> (crate `btrfs-disk`, MIT OR Apache-2.0).
Crate je vendorovaný (stejně jako crates v `macfs-plugins`), není členem workspace.

## Co je změněno

* **Odstraněn `build.rs` a build-závislost `bindgen`.** Upstream generuje `src/raw/bindings.rs` při každém buildu z hlaviček
  `btrfs.h` / `btrfs_tree.h` pomocí bindgenu, který vyžaduje nainstalovaný LLVM/libclang. Pro build pluginů ve Windows
  (MSVC) by to byla zbytečná závislost. Vygenerovaný soubor je teď přiložen hotový: `src/raw/bindings.rs`
  (vygenerováno bindgenem 0.72.1 s přenositelným shimem `linux/types.h` z upstreamu, tj. stejnou cestou, jakou
  upstream používá na ne-Linuxových platformách; výstup tedy nezávisí na tom, kde se generovalo).
* `src/raw.rs`: místo `include!(concat!(env!("OUT_DIR"), "/bindings.rs"))` je `include!("raw/bindings.rs")`.
* `Cargo.toml`: `build = false`, odstraněna sekce `[build-dependencies.bindgen]`.
* Odstraněny hlavičky `src/raw/*.h` a shim `src/raw/linux/` (nejsou potřeba, bindings.rs je odvozen z uapi hlaviček
  Linuxu, které mají výslovnou výjimku pro použití v uživatelských programech).

Žádná jiná změna kódu parseru. Při aktualizaci upstreamu: znovu vygenerovat `bindings.rs` (v kopii crate s
`if true { builder = builder.clang_arg("-Isrc/raw"); }` v build.rs) a zopakovat tyto kroky.
