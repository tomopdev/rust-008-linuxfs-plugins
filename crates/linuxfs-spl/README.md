# linuxfs-spl – plugin (btrfs, ext2/3/4, SquashFS) pro Altap Salamander 4.0 / Open Salamander 5.0 (64 bit, jen čtení)

Součást workspace [linuxfs-plugins](../../README.md). Plugin typu „panel archiver“ (`FUNCTION_PANELARCHIVERVIEW`) s ručně psanými vtabulkami
Salamander SDK (`spl.rs`), registrovaný na přípony `btrfs;ext2;ext3;ext4;squashfs;sqfs;sfs;img;raw;dd;linuxfsdev`.

* výpis (`ListArchive`) a rozbalení (`UnpackArchive`, `UnpackOneFile`) s dialogem průběhu a zrušením,
* panel Salamanderu používá ANSI jména, plugin proto ukazuje bezpečná ANSI jména a při rozbalení je mapuje zpět na původní položky,
* dotaz na přepsání existujícího souboru se klade jednou; chyby se hlásí zprávou, nikdy pádem (`catch_unwind`),
* chyba jedné položky nezastaví zbytek: výpis i rozbalení pokračují a na konci je shrnutí v okně a úplný seznam v `%TEMP%\linuxfs-errors-list.txt` / `linuxfs-errors-unpack.txt` (položky, které Salamander odmítne kvůli cestě od ~260 znaků, také).

Instalace: `linuxfs.spl` do `plugins\linuxfs\`, přidat v Plugins Manageru. Testy (mock Salamanderu nad fixtures btrfs, ext a SquashFS): `cargo test -p linuxfs-spl --release`.