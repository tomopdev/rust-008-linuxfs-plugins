# linuxfs-wcx – plugin (btrfs, ext2/3/4, SquashFS, XFS) pro Total Commander (64 bit, jen čtení)

Součást workspace [linuxfs-plugins](../../README.md). Plugin WCX (Unicode exporty `...W`, `PK_CAPS_BY_CONTENT`, `CanYouHandleThisFileW`)
nad `linuxfs-core`.

* otevře obraz, oddíl nebo disk s btrfs, ext2/3/4, SquashFS nebo XFS (holý svazek, GPT, MBR); poznává podle obsahu, ne podle přípony,
* více oddílů = složky `Partition N - název`; podsvazky btrfs jsou složky,
* skutečný disk: textový soubor `*.linuxfsdev` (`device=`, `offset=`, `partition=`, `subvol=` / `subvolid=`),
* extrakce s průběhem a přerušením (nedokončený soubor se smaže), zachová čas změny, dlouhé cesty (`\\?\`),
* chyba jedné položky nezastaví zbytek: problémy při výpisu se ohlásí při zavření archivu otevřeného pro výpis, problémy při rozbalení po rozbalení (okno + `%TEMP%\linuxfs-errors-list.txt` / `linuxfs-errors-unpack.txt`),
* soubor, který není podporovaný svazek, vrací `E_UNKNOWN_FORMAT`, takže ho TC nabídne jiným pluginům.

Instalace: `dist\linuxfs_wcx.zip` (obsahuje `pluginst.inf`). Pozor: pluginy TC nesmí padat – všechny exporty jsou obalené `catch_unwind`.
Testy: `cargo test -p linuxfs-wcx --release`.
