# Lokální úpravy crate `ext4-view` 1.0.0

Upstream: https://github.com/nicholasbishop/ext4-view-rs (Google LLC, licence MIT OR Apache-2.0, texty v LICENSE-MIT a LICENSE-APACHE).
Zdrojové soubory jsou jinak beze změny (jen byl vynechán Cargo.lock, aby workspace měl jediný).

## 1. Řídké soubory na ext2 / ext3 (nepřímé bloky)

`src/iters/file_blocks/block_map.rs`, `IndirectBlockIter::new`: u souboru s díry na úrovni nepřímého bloku (ukazatel
v inodu nebo v nadřazeném nepřímém bloku je 0) crate četl „nepřímý blok číslo 0“ a čtení souboru skončilo chybou
`corrupt filesystem: invalid read of length N from block 0`. Nulový ukazatel je podle formátu díra, takže se teď nic nečte
a blok se bere jako samé nuly (= díra, soubor se přečte s nulami). Týká se ext2/3 a ext4 bez příznaku `extents`.
Test: `crates/linuxfs-core/tests/ext.rs` (obraz ext2/ext3 obsahuje `sparse.bin` se dvěma děrami; bez opravy čtení selže).
Až oprava vyjde upstream, lze adresář `vendor/ext4-view` smazat a vrátit závislost na crates.io.

## 2. Poškozený blok adresáře neukončí celý výpis

`src/iters/read_dir.rs`: upstream po první chybě uvnitř adresáře (špatný kontrolní součet bloku, vadná položka, nulový blok)
iterátor ukončí a zbytek adresáře se nedá vypsat. `ReadDir` teď chybu vrátí **a pokračuje dalším blokem**; chyba při
zjišťování dalšího bloku (rozbitý strom extentů adresáře) výpis dál ukončuje. Volající (`ExtVolume::list_directory`) každou
chybu zaznamená jako upozornění, takže nic nezmizí potichu.
Test: `damaged_image_keeps_listing_and_reports_problems` v `crates/linuxfs-core/tests/ext.rs`.

Související změna v `src/dir.rs` (`get_dir_entry_inode_by_name`, lineární hledání jména v adresáři bez HTree): chyba v jednom
bloku už nezastaví hledání v dalších blocích; zapamatovaná chyba se vrátí, jen když jméno nikde není. Bez toho by se po
poškození jednoho bloku nedaly otevřít ani soubory, které jsou v následujících blocích a jsou ve výpisu vidět.
