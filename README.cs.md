# linuxfs-plugins

*[English](README.en.md)*

**Prohlížení a rozbalování linuxových souborových systémů (btrfs, ext2/3/4, SquashFS, XFS, EROFS) v Total Commanderu a Altap/Open Salamanderu. Jen pro čtení, napsáno v Rustu.**

![license](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)
![rust](https://img.shields.io/badge/Rust-edition%202024-orange)
![platform](https://img.shields.io/badge/platforma-Windows%20x64-lightgrey)
![mode](https://img.shields.io/badge/režim-jen%20pro%20čtení-green)

Pluginy ukážou obraz disku, oddíl nebo celý fyzický disk s Linuxem jako obyčejný archiv: procházíš složky, prohlížíš soubory (F3)
a rozbaluješ je (F5). Nic se nikdy nezapisuje. Projekt navazuje na macfs-plugins (stejná architektura) a další
souborové systémy se budou přidávat postupně.

* `linuxfs_wcx.wcx64` – plugin **WCX** pro Total Commander 11.x (64 bit)
* `linuxfs.spl` – plugin pro **Altap Salamander 4.0 / Open Salamander 5.0** (64 bit)

Oba pluginy sdílejí stejné jádro (`linuxfs-core`), takže umí totéž.

> **Stav:** projekt vznikl a byl testován v Linuxu na **obrazech vyrobených skutečným Linuxem** (btrfs: `mkfs.btrfs` + mount, obsah
> v testech porovnán s tím, co hlásí jádro; ext2/3/4: `mke2fs -d` ze zdrojového adresáře, kontrola `e2fsck`; SquashFS: `mksquashfs` ze zdrojového adresáře; XFS: `mkfs.xfs` + mount, obsah porovnán s tím, co hlásí jádro, v4 naplněné přes `mkfs.xfs -p`) a na pluginovém rozhraní přes zkušební „hostitele“.
> Ruční testy v reálném Total Commanderu a Salamanderu prošly pro btrfs (oddíly na jednom disku, oddíl s 8 podsvazky). Podpora **ext2/3/4**, **SquashFS**, **XFS**
> a nové **hlášení chyb** jsou ověřené jen automatickými testy; sestavení pro Windows (MSVC) se musí provést u tebe – viz [Omezení](#omezení-a-neověřené-věci).

## Obsah

- [Co je implementováno](#co-je-implementováno)
- [Chyby a částečné chyby](#chyby-a-částečné-chyby)
- [Instalace](#instalace)
- [Použití](#použití)
- [Sestavení](#sestavení)
- [Struktura repozitáře](#struktura-repozitáře)
- [Jak to funguje](#jak-to-funguje)
- [Testy](#testy)
- [Omezení a neověřené věci](#omezení-a-neověřené-věci)
- [Zápis na ext4: je to možné a co by to stálo](#zápis-na-ext4-je-to-možné-a-co-by-to-stálo)
- [Co zatím není implementováno (roadmap)](#co-zatím-není-implementováno-roadmap)
- [Licence](#licence)

## Co je implementováno

| Oblast | Formát | Stav | Poznámka |
|---|---|---|---|
| Souborový systém | **btrfs** | ✅ jen čtení | vlastní vrstva nad vendorovaným crate `btrfs-disk` 0.13.0 (struktury na disku), čtení B-stromů, extentů, podsvazků |
| | **ext2, ext3, ext4** | ✅ jen čtení | vrstva nad vendorovaným crate `ext4-view` 1.0.0 (extenty i nepřímé bloky, HTree, 64bit, `metadata_csum`); druh se pozná podle příznaků jako v jádře |
| | **SquashFS** 4.0 | ✅ jen čtení | crate `rust-fs-squashfs` 0.4.0 (čistý Rust, MIT) přes vlastní adaptér nad `DiskSource`; komprese **gzip, xz, lzo, lz4, zstd** (i starý lzma), fragmenty, řídké soubory; položky se adresují odkazem na inode |
| | **XFS** v4 i v5 | ✅ jen čtení | crate `rust-fs-xfs` 0.14.2 (čistý Rust, MIT) přes vlastní adaptér nad `DiskSource`; na v5 se ověřují kontrolní součty a identita metadat; `bigtime`, `reflink`, `rmapbt`, `finobt`, sparse inody, všechny tvary adresářů, B+strom extentů; položky se adresují skutečným číslem inode; data souborů čte vlastní čtečka po extentech (zhruba 10× rychlejší než blok po bloku v crate) |
| | **EROFS** | ✅ jen čtení | crate `rust-fs-erofs` 0.5.0 (čistý Rust, MIT) přes vlastní adaptér nad `DiskSource`; compact i extended inody, data bez komprese, **LZ4 / LZMA / DEFLATE / ZSTD**, ztailpacking, fragmenty, dedupe, big_pcluster, chunkové soubory s dírami; položky se adresují číslem inode (NID); součet superbloku (CRC32C) se ověřuje; obrazy s dalšími zařízeními (`--blobdev`), 48bit a „metabox“ se odmítnou s hláškou |
| Komprese (btrfs) | zlib, **zstd**, **LZO** | ✅ | zstd přes `ruzstd`, zlib přes `flate2`, LZO vlastní bezpečný dekodér |
| Podsvazky (btrfs) | podsvazky, vnořené podsvazky, snapshoty | ✅ | top-level svazek se všemi podsvazky jako složkami; lze vybrat jen jeden (`subvol=`, `subvolid=`) |
| Žurnál (ext3/4, log XFS) | nečistě odpojený svazek | ✅ jen v paměti | žurnál / log se **nepřehrává na disk**, jen se namapuje nebo přehraje v paměti; uvidíš stav, jaký by měl svazek po obnově |
| Soubory | běžné, sparse (díry), prealloc, inline, reflink, hard linky | ✅ | velikosti nad 4 GiB ověřeny |
| | symbolické odkazy | ✅ | zobrazí se jako malý soubor s cílem odkazu |
| | fifo, sockety, zařízení | skryto | nemají ve Windows smysl |
| Oddíly | holý svazek, **GPT** (sektor 512 i 4096), **MBR** (primární i logické) | ✅ | oddíly se najdou automaticky, víc oddílů = složky `Partition N - název`; v jednom disku mohou být btrfs, ext, SquashFS, XFS i EROFS vedle sebe |
| Zdroj | obraz, fyzický disk / oddíl | ✅ | disk přes textový soubor `.linuxfsdev` |

Pro každý svazek je v kořeni virtuální soubor s popisem: `!BTRFS-INFO.txt` (označení, UUID, velikost, seznam podsvazků a výchozí podsvazek),
resp. `!EXT2-INFO.txt` / `!EXT3-INFO.txt` / `!EXT4-INFO.txt` (označení, UUID, velikosti, příznaky, stav, informace o žurnálu),
resp. `!SQUASHFS-INFO.txt` (komprese, velikost bloku, počet inodů, čas vytvoření, příznaky obrazu),
resp. `!XFS-INFO.txt` (označení, UUID, v4 / v5, velikosti, alokační skupiny, vlastnosti, stav logu),
resp. `!EROFS-INFO.txt` (označení, UUID, velikost bloku a obrazu, počet inodů, čas vytvoření, kompresory, příznaky).

Jména souborů jsou v Linuxu libovolné bajty; pluginy je převádějí tak, aby je Windows přijal (nepovolené znaky a rezervovaná jména → `_`,
neplatné UTF-8 se nahradí, shody bez ohledu na velikost písmen dostanou ` (2)`), rozbalování používá vždy původní položku, ne převedené jméno.

## Chyby a částečné chyby

**Obecný princip obou pluginů: chyba jednoho souboru, adresáře nebo oddílu nezastaví zbytek práce.** Co se nepodaří přečíst, se zapíše,
zbytek se zobrazí / rozbalí a na konci se ukáže shrnutí v okně a **úplný seznam se uloží do textového souboru** v `%TEMP%`
(UTF-8 s BOM; soubor se při další operaci stejného druhu přepíše):

| Soubor | Kdy vzniká | Co obsahuje |
|---|---|---|
| `linuxfs-errors-list.txt` | při výpisu | adresáře, které nešly vypsat; položky s nečitelným inodem; oddíly, které se nepodařilo otevřít (přeskočí se); v Salamanderu položky, které hostitel odmítl (cesta od cca 260 znaků, nepřijatelné jméno) |
| `linuxfs-errors-unpack.txt` | při rozbalování / prohlížení | soubory, které se nepodařilo přečíst nebo zapsat, a proč |

* **Salamander** má omezení cesty na 260 znaků; takové položky se přeskočí (i s obsahem) a vypíšou se v okně a v souboru. Při chybě rozbalení se jako dřív
  ptá *přeskočit / přeskočit všechny s chybou / zrušit*; každá chyba se navíc zapíše a na konci se ukáže shrnutí.
* **Total Commander** žádné omezení nemá. Chyby při výpisu se ohlásí oknem při zavření archivu otevřeného pro výpis, chyby při rozbalování oknem po rozbalení;
  chybný soubor vrátí TC chybu (nabídne přeskočit / přerušit) a nedokončený cílový soubor se vždy smaže.
* Poškozený svazek se přečte, co jde: u **ext** se po poškozeném bloku adresáře pokračuje dalším blokem a soubory z nepoškozených bloků zůstávají čitelné
  (lokální úprava `ext4-view`, viz `vendor/ext4-view/PATCHES.md`); u **btrfs** se vynechá položka s chybějícím inodem a adresář zůstane čitelný.

## Instalace

**Total Commander** – zip `dist\linuxfs_wcx.zip` obsahuje `pluginst.inf`; stačí ho otevřít v TC a potvrdit instalaci.
Přípony (např. `btrfs`, `ext4`, `squashfs`, `img`, `linuxfsdev`) se přiřadí v *Konfigurace → Nastavení → Zabalování → Konfigurovat pakovací programy*.
Plugin pozná formát podle obsahu, takže jde ručně otevřít i soubor s jinou příponou (**Ctrl+PgDn**).

**Salamander** – `linuxfs.spl` zkopíruj do složky `plugins\linuxfs\` a v *Plugins → Plugins Manager* ho přidej. Plugin se
registruje na přípony `btrfs;ext2;ext3;ext4;squashfs;sqfs;sfs;xfs;erofs;img;raw;dd;linuxfsdev`.

## Použití

* **Obraz nebo svazek**: dvojklik / Ctrl+PgDn na `.img`, `.raw`, `.btrfs`, `.ext4`, `.squashfs` / `.sqfs`, `.erofs` … Při více oddílech (GPT/MBR) je každý ve vlastní složce.
* **Skutečný disk či oddíl** (např. disk s Linuxem v USB adaptéru, nebo druhý disk v PC): textový soubor `linux.linuxfsdev`

  ```ini
  device = \\.\PhysicalDrive2
  partition = 2          ; volitelné: POŘADÍ mezi nalezenými svazky (1 = první), ne číslo oddílu na disku
  # offset = 209735680   ; nebo přesný oddíl v bajtech
  # length = ...         ; volitelná délka
  subvol = @home         ; volitelné, jen btrfs: ukázat jen tento podsvazek ("/" = top-level, "default" = výchozí)
  # subvolid = 257       ; nebo podle čísla
  ```

  Čtení surového disku vyžaduje práva správce (spusť Total Commander / Salamander jako správce). Disk se **nikdy** nezapisuje.
  Pozor: čtení **běžícího** (připojeného) svazku z jiného systému může dát nekonzistentní výsledek, pokud se na něm zrovna zapisuje;
  ext3/4 s nepřehraným žurnálem (nečisté vypnutí, hibernace) se zobrazí tak, jak by vypadal po obnově.
* U btrfs se bez `subvol=` zobrazí celý svazek (top-level, tedy i `@`, `@home`, snapshoty jako složky).

## Sestavení

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (Desktop development with C++).
Při prvním sestavení si cargo stáhne crates (`crc`, `bitflags`, … pro ext) – potřebuje internet.

```powershell
.\build.ps1                  # testy celého workspace + oba pluginy -> dist\
.\build.ps1 -Plugin wcx      # jen Total Commander (dist\linuxfs_wcx.wcx64, dist\linuxfs_wcx.zip)
.\build.ps1 -Plugin spl      # jen Salamander (dist\linuxfs.spl)
.\build.ps1 -Native          # -C target-cpu=native (nepřenosné na starší CPU)
```

Testy (`cargo test --workspace`) běží i v Linuxu/macOS; pluginy se sestavují pro `x86_64-pc-windows-msvc`.

## Struktura repozitáře

| Cesta | Obsah |
|---|---|
| `crates/linuxfs-core` | společné jádro: zdroj dat s cache (`source`), detekce svazků a oddílů (`detect`), btrfs (`btrfs`), ext2/3/4 (`ext`), SquashFS (`squashfs`), XFS (`xfs`), EROFS (`erofs`), LZO (`lzo`), jednotný pohled na svazek (`volume`), sběr chyb a zápis do souboru (`report`) |
| `crates/linuxfs-wcx` | plugin Total Commanderu |
| `crates/linuxfs-spl` | plugin Salamanderu |
| `vendor/btrfs-disk` | upstream crate s lokálními úpravami (bez bindgenu/libclang, popsány v `PATCHES.md`), není členem workspace |
| `vendor/ext4-view` | upstream crate s lokálními úpravami (řídké soubory na ext2/3, pokračování po poškozeném bloku adresáře; `PATCHES.md`), není členem workspace |
| `tools/make-btrfs-fixtures.sh` | skript (Linux, root), který vyrobí testovací obrazy btrfs a manifesty ze skutečného jádra |
| `tools/make-ext-fixtures.sh` | skript (Linux, **bez roota**, `mke2fs -d`), který vyrobí obrazy ext2/3/4 a poškozený ext4 |
| `tools/make-squashfs-fixtures.sh` | skript (Linux, **bez roota**, `mksquashfs` ze `squashfs-tools`), který vyrobí obrazy SquashFS s pěti kompresemi a poškozený obraz |
| `tools/make-xfs-fixtures.sh` | skript (Linux, root, `xfsprogs`), který vyrobí obrazy XFS v5, v5 s malými bloky, v4, s nečistým logem a poškozený, a manifesty z jádra; `mkfs.xfs` nevyrobí obraz pod 300 MB, obrazy jsou řídké a po gzipu mají asi 2 MB, testy je rozbalují řídce |
| `tools/make-erofs-fixtures.sh` | skript (Linux, `erofs-utils` >= 1.8, root jen pro kontrolu jádrem), který vyrobí obrazy EROFS (bez komprese compact / extended / 1 KiB bloky, LZ4, LZ4HC s fragmenty, LZMA, DEFLATE, ZSTD, chunkové) a dva poškozené; jako root je navíc připojí jádrem a porovná se zdrojem |
| `build.ps1` | build a testy ve Windows |

## Jak to funguje

Soubor se otevře jako `DiskSource` (čtení po blocích 128 KiB s cache). `detect` v něm najde svazky (btrfs: superblok na 64 KiB, magic `_BHRfS_M`;
ext: superblok na 1 KiB, magic `0xEF53` a kontrola rozumných hodnot) holé, v GPT nebo v MBR (včetně rozšířených oddílů). `BtrfsVolume` načte chunk tree
(převod logických adres), prochází B-stromy a čte extenty; `ExtVolume` předá `ext4-view` čtečku nad oddílem. Položky se adresují dvojicí *(strom, inode)*
(u ext je to číslo do tabulky cest, protože `ext4-view` adresuje cestou; u SquashFS je to přímo odkaz na inode, magie `hsqs` + kontrola verze a velikosti bloku); u XFS je to skutečné číslo inode, magie `XFSB` + kontrola velikostí bloku, sektoru a inodu a verze 4 / 5); u EROFS je to číslo inode (NID), magie `0xE0F5E1E2` na +1024 B + kontrola velikosti bloku), ne jménem. Pluginy jsou na formátu nezávislé, jen překládají jednotné rozhraní
*vypiš složku / přečti soubor* na API Total Commanderu, resp. Salamanderu. Čtení je líné a souborová data se streamují.

## Testy

```bash
cargo test --workspace --release
# obrazy v tests/fixtures lze znovu vyrobit: tools/make-btrfs-fixtures.sh (root, btrfs-progs), tools/make-ext-fixtures.sh (e2fsprogs), tools/make-squashfs-fixtures.sh (squashfs-tools), tools/make-xfs-fixtures.sh (root, xfsprogs), tools/make-erofs-fixtures.sh (erofs-utils)
```

Obrazy btrfs vytvořil skutečný Linux a každý soubor má v `*.manifest` velikost, čas a SHA-256 podle jádra; testy čtečku porovnávají s tímto manifestem
(včetně komprese zlib/zstd/LZO, sparse souborů, 40 úrovní adresářů, 3000 souborů ve složce při uzlech 4 KiB, snapshotů a vnořených podsvazků).
Obrazy `ext2`, `ext3`, `ext4` vytvořil `mke2fs -d` ze zdrojového adresáře (1 KiB / 4 KiB bloky, žurnál, extenty, `metadata_csum`, 3000 souborů v adresáři, hluboké
stromy, symlinky rychlé i pomalé, neplatné UTF-8 ve jménu, sparse soubor s dírami v nepřímých blocích); manifest vychází ze zdroje a `e2fsck -fn` ověřil konzistenci obrazu.
`ext4-damaged` má vynulovaný inode, vynulovaný adresář a jeden vynulovaný blok velkého adresáře a ověřuje, že se zbytek přečte a že se chyby ohlásí.
Obrazy SquashFS (`squashfs-gzip`, `-xz`, `-zstd`, `-lz4`, `-lzo`) vyrobil `mksquashfs` z jednoho zdrojového adresáře (jiná komprese a velikost bloku 4 KiB – 1 MiB, s fragmenty i bez, s export tabulkou i bez; 1500 souborů v adresáři přes
víc metadatových bloků, 200 malých souborů ve fragmentech, řídký soubor, hluboký strom, symlinky, neplatné UTF-8 ve jménu); společný `squashfs.manifest` je ze zdroje. `squashfs-damaged` má zničený komprimovaný
blok dat jednoho souboru a první metadatový blok adresářů a ověřuje, že se zbytek přečte a že se chyby ohlásí.
Obrazy XFS vyrobil `mkfs.xfs` a naplnilo je jádro přes mount, manifesty jsou to, co hlásí jádro: `xfs-v5` (4 KiB bloky, reflink, rmapbt, bigtime, časy před rokem 1970 a po roce 2038, soubor s 2500 extenty = B+strom extentů, 3000 souborů v adresáři, symlinky v inodu i v blocích, prealokovaný soubor, reflinkovaná kopie),
`xfs-v5-small` (1 KiB bloky, adresářové bloky 4 KiB, bez reflink / bigtime), `xfs-v4` (bez kontrolních součtů, bez typu souboru v adresářích, inody 256 B, naplněný přes `mkfs.xfs -p`; jeho mtime se neporovnává), `xfs-dirty` (log s neprovedenými změnami – přejmenování, smazání, nové soubory – porovnaný se stavem, který po obnově hlásí jádro; test ověří i to, že se obraz nezmění)
a `xfs-damaged` (tři zničené inody a zničený blok adresáře; zbytek se musí přečíst a chyby ohlásit). `xfs_repair -n` ověřil každý obraz.
Obrazy EROFS (`erofs-plain`, `-plain-1k`, `-extended`, `-lz4`, `-lz4hc`, `-lzma`, `-deflate`, `-zstd`, `-chunked`) vyrobil `mkfs.erofs` 1.9 z jednoho zdrojového adresáře (compact i extended inody, bloky 1 a 4 KiB, ztailpacking, fragmenty, dedupe, chunkové soubory s dírami, 1500 souborů v adresáři přes víc bloků, 200 malých souborů, hluboký strom, hard linky, symlinky, neplatné UTF-8 ve jménu); každý obraz prošel `fsck.erofs` a připojení jádrem (jako root) ukázalo přesně obsah zdroje, společný `erofs.manifest` je ze zdroje. `erofs-damaged` má zničené tři inody a první blok adresáře, `erofs-damaged-lz4` zničený komprimovaný proud jednoho souboru; testy ověří, že se zbytek přečte a chyby se ohlásí. Testy dále ověřují odmítnutí 48bit / metabox / dalších zařízení a špatného součtu superbloku.
Pluginové testy (`linuxfs-wcx`, `linuxfs-spl`) běží proti mock hostitelům a ověřují i okna a textové soubory s chybami.

## Omezení a neověřené věci

* Sestavení pro Windows (MSVC) tady nešlo ověřit; části jen pro Windows (okno se zprávou v pluginu TC) tedy nebyly přeložené. Ruční test v reálném TC / Salamanderu
  prošel pro btrfs; ext a hlášení chyb ještě ne.
* **Jen čtení.** Zápis není implementován (u ext4 viz [níže](#zápis-na-ext4-je-to-možné-a-co-by-to-stálo)).
* **btrfs více zařízení** (RAID0/1/10/5/6 přes více disků): čte se jen zařízení, které plugin dostal; data na dalších zařízeních se nedají přečíst.
  Kontrolní součty dat se **neověřují** (rychlost). Odmítnuto s hláškou: zoned, raid-stripe-tree, remap-tree, extent-tree-v2. Šifrované extenty (fscrypt) se nečtou.
* **ext:** odmítnuté vlastnosti (svazek se neotevře s hláškou): komprese, `meta_bg`, MMP, `ea_inode`, `dirdata`, `large_dir`, **`inline_data`**, samostatné zařízení se žurnálem
  a také ext2 **bez příznaku `filetype`** (velmi staré svazky). Šifrované (fscrypt) položky se nečtou. Metadata se ověřují kontrolními součty (`metadata_csum`), data ne.
  Žurnál se jen mapuje v paměti (neprovádí se obnova na disku).
* **SquashFS:** jen verze 4.0 (starší 1.x–3.x se nepozná); komprese gzip, xz, lzo, lz4, zstd a starý lzma (ten jen „best effort“). Obraz nemá kontrolní součty dat, takže poškození se pozná jen tam,
  kde selže dekomprese nebo kontrola struktur; zkrácený obraz se nepodaří otevřít (s hláškou). `rust-fs-squashfs` je mladý crate (0.4.0, jeden autor), správnost je ověřená jen na obrazech z `mksquashfs` (viz Testy).
  Adresáře se čtou celé najednou (listing se dekomprimuje celý), takže adresář s poškozeným metadatovým blokem se nevypíše vůbec (hlásí se jako nečitelný adresář).
* **XFS:** odmítnuto s hláškou: log na samostatném zařízení, `needsrepair`, `metadir` a neznámé příznaky vlastností, svazek se smazanými, ale ještě otevřenými soubory po pádu (inody na seznamu unlinked: jejich uvolnění by byl zápis; stačí svazek jednou připojit v Linuxu nebo spustit `xfs_repair`), nečistý log s log-incompatible vlastnostmi. Soubory na realtime zařízení se nedají přečíst (chyba jen u nich). Log se přehrává jen v paměti. Data nemají kontrolní součty. Adresář s poškozeným blokem se nevypíše vůbec (hlásí se jako nečitelný adresář). `rust-fs-xfs` je mladý crate (0.14.x, jeden autor), správnost je ověřená jen na obrazech vyrobených jádrem (viz Testy). Nová jádra už svazky v4 výchozě nepřipojí, proto má obraz v4 manifest bez času z jádra.
* **EROFS:** odmítnuto s hláškou: obraz s dalšími zařízeními (`--blobdev`, tabulka zařízení), 48bitové adresování, „metabox“, neznámé příznaky, nenulové `dirblkbits`, vadný součet superbloku. Compact inody nemají čas změny, ukáže se čas vytvoření obrazu (jako v jádře). Data nemají kontrolní součty. Komprimované soubory se čtou po blocích přes cache pclusterů crate: dobře komprimovatelná data zhruba desítky MB/s (v testu ≈ 50 MB/s u LZ4HC), nekomprimovaná i nekomprimovatelná data stovky MB/s a víc. `rust-fs-erofs` je mladý crate (0.5.0, jeden autor, od října 2026); správnost je ověřená na obrazech z `mkfs.erofs` 1.9 a jádrem Linuxu (viz Testy), obrazy z jiných generátorů nebyly zkoušeny.
* Atributy `xattr`, ACL a práva Linuxu se nezobrazují; časy jsou čas změny (mtime) převedený na UTC.
* Skutečný fyzický disk (`\\.\PhysicalDriveN`) a rozšíření MBR byly otestovány jen na obrazech.
* `ext4-view` je upstream crate verze 1.0 s lokálními úpravami; dvě z nich jsou opravy skutečných problémů (řídké soubory na ext2/3, pád výpisu po poškozeném bloku).
  Doporučené je je nahlásit upstreamu; po zapracování lze `vendor/ext4-view` zrušit.

## Zápis na ext4: je to možné a co by to stálo

**Stav: zápis není implementován a v této verzi se nezapisuje nic.** Z hlediska technické proveditelnosti ale **ano, dalo by se to přidat**, pokud by to v budoucnu bylo potřeba.
Zhodnocení (z dokumentace a zdrojů crates v říjnu 2026, nic z toho nebylo prakticky vyzkoušeno):

**Dostupné crates se zápisem**

| Crate | Licence | Dojem | Poznámka |
|---|---|---|---|
| `rust-fs-ext4` 0.8.0 (dříve `am-fs-ext4`) | MIT | **nejslibnější** | čisté Rust ext2/3/4, čtení i zápis **přes žurnál JBD2** (čtyřstupňový protokol s vyprázdněním mezi kroky, testy s vyvoláním pádu), přehrání žurnálu při připojení, `metadata_csum`, `inline_data`, xattr, ACL; bloková vrstva `rust-fs-core` (`BlockDevice`, `CallbackDevice`) jde napojit na náš `DiskSource`. **Nevyzkoušeno**, jen podle README: velmi mladé (pod tímto jménem od 6. 10. 2026, jen stovky stažení, jeden autor), nezapisuje do extent stromů hloubky ≥ 2, adresáře nad ~340 bloků dělají chybu, nepřímý „file replace“ na ext2/ext3 bez žurnálu není zapsán přes žurnál; není ověřeno, zda jde připojit opravdu jen pro čtení bez přehrání žurnálu na disk (to by porušilo záruku „nic se nezapisuje“) |
| `ext4plus` 0.1.0-rc.2 | MIT OR Apache-2.0 | slabší kandidát | tvrdý fork `ext4-view` (stejná architektura jako teď), čtení i zápis (`std::fs`-like API), `no_std`. Autor sám píše: *beta, známé chyby, při zápisu doporučeno na ramdisku*; **nezapisuje do žurnálu** (doporučuje žurnál vypnout), nepodporuje `inline_data`, `bigalloc`, externí xattr bloky |
| `ext4_rs` 1.3.3 | MIT | méně vhodný | zápis ano (vytvoření, zápis, `mkdir`, `unlink`, zkrácení), jen ext4; malá základna uživatelů, starší verze vyřazeny (yanked), dvě opravené známé chyby |
| `ext4-lwext4` 0.1.2 | MIT OR Apache-2.0 (obal) | **nevhodný** | obaluje C knihovnu *lwext4*, jejíž části (`ext4_extent.c`, `ext4_xattr.c`) jsou **GPL-2.0**; podle README crate se tyto soubory ve výchozím sestavení vynechávají a s příznaky `gpl-*` by byl výsledek GPL-2.0 – ext4 bez extentů je pro dnešní svazky prakticky nepoužitelné. Navíc potřebuje překladač C |

**Rizika (seřazeno podle závažnosti)**

1. **Poškození dat uživatele je nevratné a nesouměrné.** Čtecí plugin nemůže uškodit; zapisující plugin s jedinou chybou může potichu poškodit celý systém na disku s Linuxem
   (často jediná kopie). `ext4plus` je pre-0.1.0 s přiznanými chybami, takže by byl zdrojem rizika sám o sobě.
2. **Žurnál.** Zápis mimo žurnál (jaký umí `ext4plus`; `rust-fs-ext4` ho naopak žurnáluje, což toto riziko podstatně snižuje, ale bez praktického ověření) není odolný proti přerušení: pád Windows, odpojení USB nebo zrušení v půlce operace nechá svazek nekonzistentní
   (adresář ukazuje na nealokovaný blok apod.). Navíc svazek s **nepřehraným žurnálem** (`needs_recovery`) se nesmí zapisovat vůbec – starý žurnál by se při dalším
   připojení v Linuxu přehrál **přes** nově zapsaná data. Řešení by bylo žurnál nejdřív přehrát na disku (nová, riziková funkce, kterou `ext4-view` nemá) nebo svazek odmítnout.
3. **Připojený / hibernovaný / sdílený svazek.** Dual-boot s hibernací či rychlým startem nebo svazek připojený jinde (síť, VM) by se zapisoval pod rukama jinému jádru. Bylo by nutné
   odmítnout vše, co není čistě odpojené (`s_state`, `needs_recovery`, MMP, nedávný čas zápisu) – a spoléhat na to, že příznaky nelžou.
4. **Přístup k surovému disku ve Windows.** Zápis na `\\.\PhysicalDriveN` vyžaduje správce, zamknutí / odpojení svazku, zarovnání na sektory, vyprázdnění cache (`FlushFileBuffers`) a
   write-back cache s read-modify-write (dnes máme jen cache pro čtení po 128 KiB). Windows navíc může na „nerozpoznaný“ disk sáhnout sám.
5. **Pokrytí vlastností.** Svazek s vlastností, kterou crate nezná (`inline_data`, `bigalloc`, kvóty, `meta_bg`, MMP…), se nesmí zapisovat vůbec – nutný seznam „bezpečných“ svazků
   a odmítnutí zbytku; jinak hrozí tiché poškození. Stejně tak šifrované (fscrypt) a casefold adresáře.
6. **Konzistence metadat.** Zápis musí správně udržet bitmapy, počty volných bloků / inodů (i v záloze superbloků a deskriptorech skupin), kontrolní součty `metadata_csum`, časy a `i_blocks`,
   seznam osiřelých inodů, případně sparse-super zálohy. Chyba se projeví až `e2fsck` nebo při dalším spuštění Linuxu.
7. **Sémantika archivních API.** WCX (`PackFiles`, `DeleteFiles`) a Salamander (`PackToArchive`, `DeleteFromArchive`) mají vlastní představu o přerušení, přepsání a přejmenování; v ext4 nejsou
   přejmenování a přepsání bez žurnálu atomická. Nové soubory by dostaly vlastníka / práva / SELinux popisky, které Windows nezná (na systému s SELinux hrozí nutnost `restorecon`).

**Kdyby se to někdy dělalo, doporučený postup:** (1) nejdřív **jen obrazy a oddíly mimo systém**, nikdy živý disk bez explicitního `write = yes` v `.linuxfsdev`; (2) odmítnout cokoli, co není čisté
(bez `needs_recovery`, bez neznámých vlastností, bez MMP), případně nabídnout nejdřív kopii; (3) začít jen přidáváním a přepsáním souborů a `mkdir`, mazání a přejmenování až potom;
(4) po každé operaci v testech spouštět `e2fsck -fn` na obrazech vyrobených `mke2fs` (jako dnes pro čtení) a přidat fuzzing / přerušování uprostřed zápisu; (5) zvážit, zda místo vlastní implementace
nestačí **`wsl --mount`** (WSL2 připojí fyzický disk s ext4/btrfs přímo nativním linuxovým ovladačem včetně zápisu, žurnálu a obnovy) – pro zápis je to bezpečnější než jakýkoli crate v uživatelském prostoru.
Hrubý odhad (neověřený): minimální zápis nových souborů a složek na čistý ext4 bez žurnálu s testy ≈ 1–2 týdny; s bezpečným přehráním žurnálu a odolností proti přerušení zhruba násobně víc.

## Co zatím není implementováno (roadmap)

Odhady jsou **hrubé, neověřené** (jeden zkušený vývojář, včetně testů a dokumentace, s fixtures vyrobenými skutečným jádrem).

| Formát / funkce | Co by to obnášelo | Odhad |
|---|---|---|
| ext: **`inline_data`**, `meta_bg`, MMP, `bigalloc` | doplnit / opravit v `vendor/ext4-view` (nebo čekat na upstream) | dny až týdny podle vlastnosti |
| ext: přehrání žurnálu při čtení z obrazu | už se řeší mapováním v paměti; zápis viz výše | – |
| **XFS** | AG, B+stromy inodů, extenty, krátké/dlouhé formy adresářů (v5 i v4) | 1–2 týdny |
| **F2FS** | checkpointy, NAT/SIT, inline data, komprese | ≈ 1 týden |
| **LVM2 (PV/LV)** | metadata VG, lineární/striped svazky, thin později; pak se použije stávající FS | 4–7 dní |
| **mdraid** | sestavení pole z více zařízení (RAID0/1), pak se použije stávající FS | 4–6 dní |
| **LUKS1/2** | PBKDF2/Argon2, AES-XTS, dialog pro heslo | 1–2 týdny |
| btrfs: **více zařízení** | sestavení ze všech zařízení svazku (`.linuxfsdev` s více `device=`), RAID0/1/10 | 3–5 dní |
| btrfs: **RAID5/6** | rekonstrukce z parity | ≈ 1 týden |
| btrfs: ověření **checksumů** | crc32c / xxhash / sha256 / blake2 (kód v `btrfs-disk` je) | 1–2 dny |
| btrfs: **zápis** | transakce, alokace extentů, B-strom s COW, žurnál prostoru – riskantní, výrazně nejtěžší | týdny; nedoporučuje se bez rozsáhlých testů |
| ext4: **zápis** | viz [výše](#zápis-na-ext4-je-to-možné-a-co-by-to-stálo) | 1–2 týdny minimum, rizikové |
| **ZFS**, **bcachefs** | velké a složité, zatím mimo plán | týdny až měsíce |

Další souborové systémy se přidávají jako nový modul v `linuxfs-core` (rozpoznání v `detect`, výpis a čtení za rozhraním `LinuxVolume`);
pluginy se měnit nemusí.

## Licence

Projekt je šířen pod **MIT nebo Apache-2.0, podle tvé volby** ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)); dřív byl GPL-3.0-or-later kvůli kontinuitě s macfs-plugins.
Žádná použitá knihovna GPL nevyžaduje (MIT / Apache-2.0 / BSD / Zlib / BSL-1.0), seznam je v [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
Crate FrankenFS nebyl použit kvůli jeho licenčnímu dodatku vylučujícímu určité subjekty; `ext4-lwext4` kvůli GPL částem lwext4.

## Poděkování

Crates `btrfs-disk` (projekt rustutils/btrfsutils), `ext4-view` (Nicholas Bishop, Google), `rust-fs-squashfs`, `rust-fs-xfs` a `rust-fs-erofs` (antimatter-studios), `ruzstd`, `flate2`, dokumentace btrfs a ext4 (kernel.org) a projekt macfs-plugins, ze kterého je převzata struktura pluginů.
