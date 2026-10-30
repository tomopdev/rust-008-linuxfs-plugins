# linuxfs-plugins

*[English](README.en.md)*

**Prohlížení a rozbalování linuxových souborových systémů (zatím btrfs) v Total Commanderu a Altap/Open Salamanderu. Jen pro čtení, napsáno v Rustu.**

![license](https://img.shields.io/badge/licence-GPL--3.0--or--later-blue)
![rust](https://img.shields.io/badge/Rust-edition%202024-orange)
![platform](https://img.shields.io/badge/platforma-Windows%20x64-lightgrey)
![mode](https://img.shields.io/badge/režim-jen%20pro%20čtení-green)

Pluginy ukážou obraz disku, oddíl nebo celý fyzický disk s Linuxem jako obyčejný archiv: procházíš složky, prohlížíš soubory (F3)
a rozbaluješ je (F5). Nic se nikdy nezapisuje. Projekt navazuje na macfs-plugins (stejná architektura) a další
souborové systémy se budou přidávat postupně.

* `linuxfs_wcx.wcx64` – plugin **WCX** pro Total Commander 11.x (64 bit)
* `linuxfs.spl` – plugin pro **Altap Salamander 4.0 / Open Salamander 5.0** (64 bit)

Oba pluginy sdílejí stejné jádro (`linuxfs-core`), takže umí totéž.

> **Stav:** projekt vznikl a byl testován v Linuxu na **obrazech btrfs vyrobených skutečným jádrem Linuxu** (`mkfs.btrfs` + mount, obsah
> v testech porovnán s tím, co hlásí jádro) a na pluginovém rozhraní přes zkušební „hostitele“. Sestavení pro Windows (MSVC) a běh v reálném
> Total Commanderu / Salamanderu **nebyly v místě vývoje ověřeny** – viz [Omezení](#omezení-a-neověřené-věci).

## Obsah

- [Co je implementováno](#co-je-implementováno)
- [Instalace](#instalace)
- [Použití](#použití)
- [Sestavení](#sestavení)
- [Struktura repozitáře](#struktura-repozitáře)
- [Jak to funguje](#jak-to-funguje)
- [Testy](#testy)
- [Omezení a neověřené věci](#omezení-a-neověřené-věci)
- [Co zatím není implementováno (roadmap)](#co-zatím-není-implementováno-roadmap)
- [Licence](#licence)

## Co je implementováno

| Oblast | Formát | Stav | Poznámka |
|---|---|---|---|
| Souborový systém | **btrfs** | ✅ jen čtení | vlastní vrstva nad vendorovaným crate `btrfs-disk` 0.13.0 (struktury na disku), čtení B-stromů, extentů, podsvazků |
| Komprese | zlib, **zstd**, **LZO** | ✅ | zstd přes `ruzstd`, zlib přes `flate2`, LZO vlastní bezpečný dekodér |
| Podsvazky | podsvazky, vnořené podsvazky, snapshoty | ✅ | top-level svazek se všemi podsvazky jako složkami; lze vybrat jen jeden (`subvol=`, `subvolid=`) |
| Soubory | běžné, sparse (díry), prealloc, inline, reflink, hard linky | ✅ | velikosti nad 4 GiB ověřeny |
| | symbolické odkazy | ✅ | zobrazí se jako malý soubor s cílem odkazu |
| | fifo, sockety, zařízení | skryto | nemají ve Windows smysl |
| Oddíly | holý svazek, **GPT** (sektor 512 i 4096), **MBR** (primární i logické) | ✅ | oddíly se najdou automaticky, víc oddílů = složky `Partition N - název` |
| Zdroj | obraz, fyzický disk / oddíl | ✅ | disk přes textový soubor `.linuxfsdev` |

Pro každý svazek je v kořeni virtuální soubor `!BTRFS-INFO.txt` (označení, UUID, velikost, seznam podsvazků a výchozí podsvazek).

Jména souborů jsou v btrfs libovolné bajty; pluginy je převádějí tak, aby je Windows přijal (nepovolené znaky a rezervovaná jména → `_`,
neplatné UTF-8 se nahradí, shody bez ohledu na velikost písmen dostanou ` (2)`), rozbalování používá vždy původní položku, ne převedené jméno.

## Instalace

**Total Commander** – zip `dist\linuxfs_wcx.zip` obsahuje `pluginst.inf`; stačí ho otevřít v TC a potvrdit instalaci.
Přípony (např. `btrfs`, `img`, `linuxfsdev`) se přiřadí v *Konfigurace → Nastavení → Zabalování → Konfigurovat pakovací programy*.
Plugin pozná formát podle obsahu, takže jde ručně otevřít i soubor s jinou příponou (**Ctrl+PgDn**).

**Salamander** – `linuxfs.spl` zkopíruj do složky `plugins\linuxfs\` a v *Plugins → Plugins Manager* ho přidej. Plugin se
registruje na přípony `btrfs;img;raw;dd;linuxfsdev`.

## Použití

* **Obraz nebo svazek**: dvojklik / Ctrl+PgDn na `.img`, `.raw`, `.btrfs` … Při více oddílech (GPT/MBR) je každý ve vlastní složce.
* **Skutečný disk či oddíl** (např. disk s Linuxem v USB adaptéru, nebo druhý disk v PC): textový soubor `linux.linuxfsdev`

  ```ini
  device = \\.\PhysicalDrive2
  partition = 2          ; volitelné: POŘADÍ mezi nalezenými btrfs svazky (1 = první), ne číslo oddílu na disku
  # offset = 209735680   ; nebo přesný oddíl v bajtech
  # length = ...         ; volitelná délka
  subvol = @home         ; volitelné: ukázat jen tento podsvazek ("/" = top-level, "default" = výchozí)
  # subvolid = 257       ; nebo podle čísla
  ```

  Čtení surového disku vyžaduje práva správce (spusť Total Commander / Salamander jako správce). Disk se **nikdy** nezapisuje.
  Pozor: čtení **běžícího** (připojeného) svazku z jiného systému může dát nekonzistentní výsledek, pokud se na něm zrovna zapisuje.
* Bez `subvol=` se zobrazí celý svazek (top-level, tedy i `@`, `@home`, snapshoty jako složky).

## Sestavení

Požadavky: Rust ≥ 1.97, toolchain `stable-x86_64-pc-windows-msvc`, Visual Studio Build Tools (Desktop development with C++).

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
| `crates/linuxfs-core` | společné jádro: zdroj dat s cache (`source`), detekce svazků a oddílů (`detect`), btrfs (`btrfs`), LZO (`lzo`), jednotný pohled na svazek (`volume`) |
| `crates/linuxfs-wcx` | plugin Total Commanderu |
| `crates/linuxfs-spl` | plugin Salamanderu |
| `vendor/btrfs-disk` | upstream crate s lokálními úpravami (bez bindgenu/libclang, popsány v `PATCHES.md`), není členem workspace |
| `tools/make-btrfs-fixtures.sh` | skript (Linux, root), který vyrobí testovací obrazy a manifesty ze skutečného jádra |
| `build.ps1` | build a testy ve Windows |

## Jak to funguje

Soubor se otevře jako `DiskSource` (čtení po blocích 128 KiB s cache). `detect` v něm najde btrfs svazky (superblok na 64 KiB, magic `_BHRfS_M`)
holé, v GPT nebo v MBR (včetně rozšířených oddílů). `BtrfsVolume` načte chunk tree (převod logických adres), prochází B-stromy a čte extenty;
položky se adresují dvojicí *(strom, inode)*, ne cestou. Pluginy jsou na formátu nezávislé, jen překládají jednotné rozhraní
*vypiš složku / přečti soubor* na API Total Commanderu, resp. Salamanderu. Čtení je líné a souborová data se streamují.

## Testy

```bash
cargo test --workspace --release
# obrazy v tests/fixtures lze znovu vyrobit (Linux, root, btrfs-progs): tools/make-btrfs-fixtures.sh
```

Obrazy (`btrfs-main`, `btrfs-small`) vytvořil skutečný Linux a každý soubor má v `*.manifest` velikost, čas a SHA-256 podle jádra;
testy čtečku porovnávají s tímto manifestem (včetně komprese zlib/zstd/LZO, sparse souborů, 40 úrovní adresářů, 3000 souborů ve složce
při uzlech 4 KiB, snapshotů a vnořených podsvazků).

## Omezení a neověřené věci

* Sestavení pro Windows a běh v reálném TC / Salamanderu nebyl ověřen; testy běží v Linuxu na „hostitelských“ simulacích API.
* **Jen čtení.** Zápis není implementován.
* **Více zařízení** (btrfs RAID0/1/10/5/6 přes více disků): čte se jen zařízení, které plugin dostal; data uložená na dalších zařízeních
  se nedají přečíst (DUP metadat na jednom disku je ověřeno).
* Kontrolní součty dat se **neověřují** (rychlost); poškozená data se tedy nepoznají.
* Nepodporováno (svazek se odmítne s hláškou): zoned, raid-stripe-tree, remap-tree, extent-tree-v2. Šifrované extenty (fscrypt) se nečtou.
* Atributy `xattr`, ACL a práva Linuxu se nezobrazují; časy jsou čas změny (mtime) převedený na UTC.
* Skutečný fyzický disk (`\\.\PhysicalDriveN`) a rozšíření MBR byly otestovány jen na obrazech.

## Co zatím není implementováno (roadmap)

Odhady jsou **hrubé, neověřené** (jeden zkušený vývojář, včetně testů a dokumentace, s fixtures vyrobenými skutečným jádrem).

| Formát / funkce | Co by to obnášelo | Odhad |
|---|---|---|
| **ext2/3/4** | superblok, skupiny bloků, extenty (ext4) i nepřímé bloky (ext2/3), HTree adresáře, inline data, 64bit; crate `ext4-view` je vhodný kandidát | 3–5 dní |
| **XFS** | AG, B+stromy inodů, extenty, krátké/dlouhé formy adresářů (v5 i v4) | 1–2 týdny |
| **F2FS** | checkpointy, NAT/SIT, inline data, komprese | ≈ 1 týden |
| **LVM2 (PV/LV)** | metadata VG, lineární/striped svazky, thin později; pak se použije stávající FS | 4–7 dní |
| **mdraid** | sestavení pole z více zařízení (RAID0/1), pak se použije stávající FS | 4–6 dní |
| **LUKS1/2** | PBKDF2/Argon2, AES-XTS, dialog pro heslo | 1–2 týdny |
| btrfs: **více zařízení** | sestavení ze všech zařízení svazku (`.linuxfsdev` s více `device=`), RAID0/1/10 | 3–5 dní |
| btrfs: **RAID5/6** | rekonstrukce z parity | ≈ 1 týden |
| btrfs: ověření **checksumů** | crc32c / xxhash / sha256 / blake2 (kód v `btrfs-disk` je) | 1–2 dny |
| btrfs: **zápis** (vytvoření/zápis souboru) | transakce, alokace extentů, B-strom s COW, žurnál prostoru – riskantní, výrazně nejtěžší | týdny; nedoporučuje se bez rozsáhlých testů |
| **ZFS**, **bcachefs** | velké a složité, zatím mimo plán | týdny až měsíce |

Další souborové systémy se přidávají jako nový modul v `linuxfs-core` (rozpoznání v `detect`, výpis a čtení za rozhraním `LinuxVolume`);
pluginy se měnit nemusí.

## Licence

Projekt je šířen pod **GNU General Public License v3.0 nebo novější** ([LICENSE](LICENSE)). Žádná použitá knihovna GPL nevyžaduje
(MIT / Apache-2.0 / BSD / Zlib / BSL-1.0), seznam je v [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
Crate FrankenFS nebyl použit kvůli jeho licenčnímu dodatku vylučujícímu určité subjekty.

## Poděkování

Crate `btrfs-disk` (projekt rustutils/btrfsutils), `ruzstd`, `flate2`, dokumentace btrfs (kernel.org) a projekt macfs-plugins, ze kterého je převzata struktura pluginů.
