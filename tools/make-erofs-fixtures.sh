#!/usr/bin/env bash
# Vyrobí testovací obrazy EROFS pro crate linuxfs-core a manifest s obsahem zdrojového adresáře.
#
# Potřebuje `mkfs.erofs` a `fsck.erofs` z erofs-utils >= 1.8 (zstd; vyzkoušeno s 1.9, s podporou lz4 / lz4hc / lzma / deflate / zstd),
# python3 a gzip; root ani mount nepotřebuje, ale jako root (a s EROFS v jádře) se každý obraz navíc připojí jádrem a to, co jádro
# vidí, se porovná se zdrojem, takže je obraz ověřený nezávisle na naší čtečce. Reference je zdrojový adresář (velikosti, mtime, sha256).
# Výsledek: crates/linuxfs-core/tests/fixtures/erofs-*.img.gz + společný erofs.manifest (formát jako u ext / btrfs / squashfs):
#   erofs-plain        bez komprese, bloky 4 KiB, compact inody (čas změny = čas obrazu, přepínač -T)
#   erofs-plain-1k     bez komprese, bloky 1 KiB (víc bloků v adresářích a souborech)
#   erofs-extended     bez komprese, extended inody s vlastním časem změny každé položky (--preserve-mtime)
#   erofs-lz4          LZ4 (rychlý), pclustery po jednom bloku, bez ztailpackingu (komprimovaná data jsou mimo blok superbloku, takže jejich
#                      poškození nerozbije kontrolní součet superbloku, který kryje celý blok se superblokem)
#   erofs-lz4hc        LZ4HC, pcluster 64 KiB, ztailpacking + fragments + dedupe
#   erofs-lzma         LZMA (COMPR_CFGS se slovníkem), pcluster 128 KiB
#   erofs-deflate      DEFLATE
#   erofs-zstd         ZSTD, pcluster 64 KiB
#   erofs-chunked      chunkové soubory (--chunksize), řídký soubor = díry
#   erofs-damaged      kopie erofs-plain se zničenými inody (/big.bin, /names/Case.txt, /deep/level1/level2/level3) a prvním
#                      blokem adresáře /many; čtečka musí vypsat zbytek svazku a poškození ohlásit
#   erofs-damaged-lz4  kopie erofs-lz4 se zničeným komprimovaným proudem souboru /0-compressible.txt
set -euo pipefail
OUT="${1:-$(dirname "$0")/../crates/linuxfs-core/tests/fixtures}"
OUT="$(cd "$OUT" && pwd)"
WORK="$(mktemp -d)"; trap 'umount "$WORK/mnt" 2>/dev/null || true; rm -rf "$WORK"' EXIT
mkdir "$WORK/mnt"

for t in mkfs.erofs fsck.erofs dump.erofs python3 gzip; do
    command -v "$t" >/dev/null || { echo "chybí $t (balík erofs-utils >= 1.8)" >&2; exit 1; }
done

rnd() { python3 - "$1" "$2" <<'PY'
import random, sys
sys.stdout.buffer.write(random.Random(int(sys.argv[1])).randbytes(int(sys.argv[2])))
PY
}

populate() { # $1 = adresář
    local M="$1"
    # soubor, který se komprimuje skvěle a leží v obrazu jako první datový blok (řadí se podle jména): na něm se dělá poškození
    python3 - "$M" <<'PY'
import sys
with open(sys.argv[1] + "/0-compressible.txt", "wb") as f:
    f.write((b"The quick brown fox jumps over the lazy dog. 0123456789\n" * 6000)[:300000])
PY
    printf 'Hello, erofs!\n' > "$M/hello.txt"
    : > "$M/empty.bin"
    rnd 1 400000 > "$M/big.bin"                        # nekomprimovatelné: pclustery se ukládají nekomprimované
    python3 - "$M" <<'PY'
import sys
m = sys.argv[1]
with open(m + "/sparse.bin", "wb") as f:               # velké díry: u chunkových souborů se ukládají jako díry, jinde jako nuly
    f.seek(100000); f.write(b"A" * 4096)
    f.seek(5 * 1024 * 1024); f.write(b"B" * 4096)
    f.truncate(8 * 1024 * 1024)
PY
    ln "$M/hello.txt" "$M/hard1.txt"
    ln -s hello.txt "$M/link-rel"
    ln -s /etc/passwd "$M/link-abs"
    ln -s "$(python3 -c 'print("d/" * 60 + "target")')" "$M/link-long"
    mkfifo "$M/fifo"
    mkdir "$M/empty-dir"
    mkdir "$M/names"
    for n in "Příliš žluťoučký kůň.txt" "日本語.txt" 'a:b?c.txt' 'CON' 'trailing.' 'Case.txt' 'case.txt'; do
        printf 'name: %s\n' "$n" > "$M/names/$n"
    done
    printf 'x' > "$M/names/$(printf 'bad\xffutf8.bin')"
    touch "$M/names/$(python3 -c 'print("x" * 255)')"
    mkdir "$M/small"                                   # malé soubory: konce komprimovaných souborů leží v inodech a ve fragmentech
    for i in $(seq 1 200); do rnd "$i" $(( (i * 37) % 3000 + 1 )) > "$M/small/f$i.bin"; done
    mkdir "$M/many"                                    # adresář přes víc bloků
    for i in $(seq 1 1500); do printf '%s' "$i" > "$M/many/file-with-a-longer-name-$i.txt"; done
    local d="$M/deep"; mkdir "$d"
    for i in $(seq 1 30); do d="$d/level$i"; mkdir "$d"; done
    printf 'bottom\n' > "$d/leaf.txt"
    # soubory zařízení: mknod chce root; bez něj se test „zařízení se nezobrazí“ prostě obejde bez nich
    mknod "$M/chardev" c 1 3 2>/dev/null || true
}

manifest() { # $1 = adresář, $2 = výstup: typ<TAB>velikost<TAB>mtime<TAB>sha256<TAB>cesta (jen soubory, adresáře a symlinky)
    ( cd "$1"
      find . -mindepth 1 \( -type f -o -type d -o -type l \) -print0 | sort -z | while IFS= read -r -d '' p; do
          rel="${p#./}"
          if [ -L "$p" ]; then t=l; size=$(readlink -n "$p" | wc -c); sum=$(readlink -n "$p" | sha256sum | cut -d' ' -f1)
          elif [ -d "$p" ]; then t=d; size=0; sum=-
          else t=f; size=$(stat -c %s "$p"); sum=$(sha256sum < "$p" | cut -d' ' -f1); fi
          printf '%s\t%s\t%s\t%s\t%s\n' "$t" "$size" "$(stat -c %Y "$p")" "$sum" "$rel"
      done ) > "$2"
}

STAMP='2026-01-02 03:04:05 UTC'
T="$(date -u -d "$STAMP" +%s)"
SRC="$WORK/src"; mkdir "$SRC"; populate "$SRC"
find "$SRC" -exec touch -h -d "$STAMP" {} +

manifest "$SRC" "$OUT/erofs.manifest"                  # stejný zdroj pro všechny obrazy: jeden manifest

# Nezávislá kontrola: jako root se obraz připojí jádrem (loop, jen pro čtení) a to, co jádro vidí, se musí do posledního bajtu (typ, velikost,
# mtime, sha256) shodovat se zdrojem. Bez roota nebo bez EROFS v jádře se jen přeskočí (zbývá `fsck.erofs`). `fsck.erofs --extract` se
# nepoužívá: s obrazy s fragmenty (verze 1.9) selhává při ověřování „packed file“, i když je obraz v pořádku.
verify() { # $1 = obraz, $2 = název
    if [ "$(id -u)" != 0 ] || ! grep -qw erofs /proc/filesystems; then
        echo "  $2: obsah neověřen jádrem (potřebuje root a EROFS v jádře)" >&2
        return 0
    fi
    mount -t erofs -o loop,ro "$1" "$WORK/mnt" || { echo "jádro nepřipojilo $2" >&2; exit 1; }
    manifest "$WORK/mnt" "$WORK/kernel.manifest"
    umount "$WORK/mnt"
    cmp -s "$WORK/kernel.manifest" "$OUT/erofs.manifest" || { echo "obraz $2 se po připojení jádrem liší od zdroje" >&2; diff "$WORK/kernel.manifest" "$OUT/erofs.manifest" | head >&2; exit 1; }
}

# název  popisek  přepínače (různá komprese, velikost bloku, pcluster a příznaky, ať se zkusí co nejvíc cest čtečkou)
make() {
    local name="$1" label="$2"; shift 2
    local img="$WORK/erofs-$name.img"
    mkfs.erofs --quiet -U 12345678-9abc-def0-1122-334455667788 -L "$label" --all-root "$@" "$img" "$SRC" >/dev/null
    fsck.erofs "$img" >/dev/null || { echo "fsck.erofs odmítl $name" >&2; exit 1; }
    verify "$img" "$name"
    gzip -9 -c "$img" > "$OUT/erofs-$name.img.gz"
    echo "$name: $(stat -c %s "$img") B"
}
make plain      EROFS-PLAIN   -T "$T"
make plain-1k   EROFS-1K      -T "$T" -b 1024
make extended   EROFS-EXT     --preserve-mtime
make lz4        EROFS-LZ4     -T "$T" -zlz4
make lz4hc      EROFS-LZ4HC   -T "$T" -zlz4hc,9 -C 65536 -Eztailpacking,fragments,dedupe
make lzma       EROFS-LZMA    -T "$T" -zlzma,6 -C 131072
make deflate    EROFS-DEFLATE -T "$T" -zdeflate,9
make zstd       EROFS-ZSTD    -T "$T" -zzstd,3 -C 65536
make chunked    EROFS-CHUNK   -T "$T" --chunksize=65536

# Poškozené obrazy.
#  1) erofs-damaged: z erofs-plain se zničí formát tří inodů a první blok adresáře /many. Inod se najde průchodem adresářů přímo
#     v obrazu (compact i extended inody, FLAT_PLAIN i FLAT_INLINE adresáře). Nečitelný inod souboru se ukáže jako položka
#     bez údajů s upozorněním, nečitelný adresář (inod nebo blok) je chyba výpisu toho adresáře; zbytek svazku se přečte.
python3 - "$WORK/erofs-plain.img" "$WORK/erofs-damaged.img" <<'PY'
import struct, sys
img = bytearray(open(sys.argv[1], "rb").read())
bs = 1 << img[1024 + 0x0C]
root_nid = struct.unpack_from("<H", img, 1024 + 0x0E)[0]
meta = struct.unpack_from("<I", img, 1024 + 0x28)[0] * bs
def inode(nid):
    o = meta + nid * 32
    fmt, icount, mode = struct.unpack_from("<HHH", img, o)
    ext = fmt & 1
    layout = (fmt >> 1) & 7
    size = struct.unpack_from("<Q" if ext else "<I", img, o + 8)[0]
    raw_u = struct.unpack_from("<I", img, o + 0x10)[0]
    isize = 64 if ext else 32
    xsize = 0 if icount == 0 else 12 + (icount - 1) * 4
    return o, layout, size, raw_u, o + isize + xsize
def dir_blocks(nid):
    o, layout, size, raw_u, body = inode(nid)
    assert layout in (0, 2), f"directory layout {layout}"
    n = -(-size // bs)
    for i in range(n):
        if layout == 2 and i == n - 1:
            yield body, size - i * bs                  # poslední blok FLAT_INLINE leží těsně za inodem
        else:
            yield raw_u * bs + i * bs, min(bs, size - i * bs)
def entries(nid):
    for off, ln in dir_blocks(nid):
        blk = img[off:off + ln]
        n = struct.unpack_from("<H", blk, 8)[0] // 12
        offs = [struct.unpack_from("<H", blk, i * 12 + 8)[0] for i in range(n)] + [ln]
        for i in range(n):
            e_nid = struct.unpack_from("<Q", blk, i * 12)[0]
            name = bytes(blk[offs[i]:offs[i + 1]]).rstrip(b"\0") if i == n - 1 else bytes(blk[offs[i]:offs[i + 1]])
            yield name, e_nid
def find(path):
    nid = root_nid
    for part in path.strip("/").split("/"):
        nid = dict(entries(nid))[part.encode()]
    return nid
for p in ("/big.bin", "/names/Case.txt", "/deep/level1/level2/level3"):
    o = inode(find(p))[0]
    struct.pack_into("<H", img, o, 0xFFFF)             # neplatné rozložení dat (7) -> inod se nedá přečíst
many = inode(find("/many"))
first = next(iter(dir_blocks(find("/many"))))[0]
struct.pack_into("<H", img, first + 8, 0xFFFF)         # nameoff prvního záznamu mimo blok -> blok adresáře je vadný
open(sys.argv[2], "wb").write(img)
PY
gzip -9 -c "$WORK/erofs-damaged.img" > "$OUT/erofs-damaged.img.gz"

#  2) erofs-damaged-lz4: z erofs-lz4 se zničí komprimovaný proud souboru /0-compressible.txt; fyzický posun a velikost dá
#     `dump.erofs -e` (řádky s extenty). EROFS nemá kontrolní součty dat, proto se ničí jen komprimovaná data (dekomprese selže spolehlivě).
dump.erofs -e --path=/0-compressible.txt "$WORK/erofs-lz4.img" > "$WORK/extents.txt"
python3 - "$WORK/erofs-lz4.img" "$WORK/erofs-damaged-lz4.img" "$WORK/extents.txt" <<'PY'
import re, sys
img = bytearray(open(sys.argv[1], "rb").read())
txt = open(sys.argv[3]).read()
# řádek extentu: "   0:        0..  300000 |  300000 :       1456..      2699 |    1243" (logický rozsah | délka : fyzický rozsah | délka)
m = re.search(r"^\s*\d+:\s+\d+\.\.\s*\d+\s*\|\s*\d+\s*:\s*(\d+)\.\.\s*(\d+)\s*\|\s*\d+", txt, re.M)
assert m, txt
start, end = int(m.group(1)), int(m.group(2))
assert end > start + 64, (start, end)
img[start:start + 64] = b"\xff" * 64
open(sys.argv[2], "wb").write(img)
PY
gzip -9 -c "$WORK/erofs-damaged-lz4.img" > "$OUT/erofs-damaged-lz4.img.gz"
ls -la "$OUT" | grep -i erofs
