#!/usr/bin/env bash
# Vyrobí testovací obrazy SquashFS (každá komprese jedna) pro crate linuxfs-core a manifest s obsahem zdrojového adresáře.
#
# Potřebuje `mksquashfs` (balík squashfs-tools >= 4.5, s podporou gzip / xz / lzo / lz4 / zstd), nepotřebuje root ani mount.
# Reference je zdrojový adresář (velikosti, mtime, sha256), `unsquashfs -s` jen potvrdí zvolenou kompresi.
# Výsledek: crates/linuxfs-core/tests/fixtures/squashfs-{gzip,xz,zstd,lz4,lzo}.img.gz + společný squashfs.manifest (formát jako u ext / btrfs)
# a squashfs-damaged.img.gz (poškozený gzip obraz pro test „pokračovat po chybě“).
set -euo pipefail
OUT="${1:-$(dirname "$0")/../crates/linuxfs-core/tests/fixtures}"
OUT="$(cd "$OUT" && pwd)"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT

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
    printf 'Hello, squashfs!\n' > "$M/hello.txt"
    : > "$M/empty.bin"
    rnd 1 400000 > "$M/big.bin"                        # nekomprimovatelné: bloky se ukládají nekomprimované
    python3 - "$M" <<'PY'
import sys
m = sys.argv[1]
with open(m + "/sparse.bin", "wb") as f:               # nulové bloky mksquashfs ukládá jako sparse (velikost bloku 0)
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
    mkdir "$M/small"                                   # malé soubory: jejich konce leží ve fragmentech
    for i in $(seq 1 200); do rnd "$i" $(( (i * 37) % 3000 + 1 )) > "$M/small/f$i.bin"; done
    mkdir "$M/many"                                    # listing přes víc metadatových bloků (8 KiB)
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

SRC="$WORK/src"; mkdir "$SRC"; populate "$SRC"
find "$SRC" -exec touch -h -d '2026-01-02 03:04:05 UTC' {} +

manifest "$SRC" "$OUT/squashfs.manifest"               # stejný zdroj pro všechny obrazy: jeden manifest

# název  komprese  přepínače (různé velikosti bloku a příznaky, ať se zkusí co nejvíc cest čtečkou)
make() {
    local name="$1" comp="$2"; shift 2
    local img="$WORK/squashfs-$name.img"
    mksquashfs "$SRC" "$img" -comp "$comp" -quiet -no-progress -noappend -all-root "$@" >/dev/null
    unsquashfs -s "$img" | grep -qi "Compression $comp" || { echo "mksquashfs nepoužil kompresi $comp" >&2; exit 1; }
    gzip -9 -c "$img" > "$OUT/squashfs-$name.img.gz"
    echo "$name: $(stat -c %s "$img") B"
}
make gzip gzip
make xz   xz   -b 256K
make zstd zstd -b 1M
make lz4  lz4  -b 64K -Xhc
make lzo  lzo  -b 8K -no-fragments -no-exports -no-xattrs

# Poškozený obraz: z gzip obrazu se zničí komprimovaný proud prvního datového bloku (0-compressible.txt začíná hned za
# superblokem na +96) a payload prvního metadatového bloku tabulky adresářů (listingy `deep` a začátek `many`; kořen a ostatní adresáře zůstanou).
# Čtečka musí vypsat zbytek svazku a obě poškození ohlásit. SquashFS nemá kontrolní součty dat, proto se ničí jen
# komprimovaná data (u nich dekomprese selže spolehlivě).
python3 - "$WORK/squashfs-gzip.img" "$WORK/squashfs-damaged.img" <<'PY'
import struct, sys
img = bytearray(open(sys.argv[1], "rb").read())
dir_start, frag_start = struct.unpack_from("<Q", img, 0x48)[0], struct.unpack_from("<Q", img, 0x50)[0]
img[96 + 12:96 + 12 + 64] = b"\xff" * 64
hdr = struct.unpack_from("<H", img, dir_start)[0]
size, uncompressed = hdr & 0x7FFF, bool(hdr & 0x8000)
assert not uncompressed and size > 16 and dir_start + 2 + size <= frag_start, (hdr, dir_start)
img[dir_start + 2 + 4:dir_start + 2 + size] = b"\xff" * (size - 4)
open(sys.argv[2], "wb").write(img)
PY
gzip -9 -c "$WORK/squashfs-damaged.img" > "$OUT/squashfs-damaged.img.gz"
ls -la "$OUT" | grep -i squashfs
