#!/usr/bin/env bash
# Vyrobí testovací obrazy ext2 / ext3 / ext4 pro crate linuxfs-core a manifest s obsahem zdrojového adresáře.
#
# NEPOTŘEBUJE root ani mount: `mke2fs -d <adresář>` naplní obraz přímo (e2fsprogs >= 1.43). Reference je zdrojový adresář
# (velikosti, mtime, sha256), protože mke2fs zachovává časy a obsah; `e2fsck -fn` navíc ověří, že je obraz konzistentní.
# Výsledek: crates/linuxfs-core/tests/fixtures/ext{2,3,4}.img.gz + ext{2,3,4}.manifest (formát jako u btrfs).
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
    printf 'Hello, ext!\n' > "$M/hello.txt"
    : > "$M/empty.bin"
    rnd 1 1500000 > "$M/big.bin"
    python3 - "$M" <<'PY'
import sys
m = sys.argv[1]
with open(m + "/sparse.bin", "wb") as f:
    f.seek(100000); f.write(b"A" * 4096)
    f.seek(10 * 1024 * 1024); f.write(b"B" * 4096)
    f.truncate(20 * 1024 * 1024)
PY
    ln "$M/hello.txt" "$M/hard1.txt"
    ln -s hello.txt "$M/link-rel"
    ln -s /etc/passwd "$M/link-abs"
    ln -s "$(python3 -c 'print("d/" * 60 + "target")')" "$M/link-long"   # > 60 B: pomalý symlink v datovém bloku
    mkfifo "$M/fifo"
    mkdir "$M/names"
    for n in "Příliš žluťoučký kůň.txt" "日本語.txt" 'a:b?c.txt' 'CON' 'trailing.' 'Case.txt' 'case.txt'; do
        printf 'name: %s\n' "$n" > "$M/names/$n"
    done
    printf 'x' > "$M/names/$(printf 'bad\xffutf8.bin')"
    touch "$M/names/$(python3 -c 'print("x" * 255)')"
    mkdir "$M/many"                                    # víc bloků adresáře (u ext4 s dir_index = HTree)
    for i in $(seq 1 3000); do printf '%s' "$i" > "$M/many/file-with-a-longer-name-$i.txt"; done
    local d="$M/deep"; mkdir "$d"
    for i in $(seq 1 30); do d="$d/level$i"; mkdir "$d"; done
    printf 'bottom\n' > "$d/leaf.txt"
}

manifest() { # $1 = adresář, $2 = výstup: typ<TAB>velikost<TAB>mtime<TAB>sha256<TAB>cesta
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
# čas změny adresářů mke2fs přebírá ze zdroje, ale vytvoření souborů v něm ho mění: zafixovat před výrobou
find "$SRC" -exec touch -h -d '2026-01-02 03:04:05 UTC' {} +
for t in ext2 ext3 ext4; do
    IMG="$WORK/$t.img"; truncate -s 96M "$IMG"
    case $t in
        ext2) opts="-t ext2 -b 1024 -O ^resize_inode" ;;
        ext3) opts="-t ext3 -b 4096" ;;
        ext4) opts="-t ext4 -b 4096 -O 64bit,metadata_csum,extents,dir_index,flex_bg" ;;
    esac
    # shellcheck disable=SC2086
    mke2fs -q -F $opts -L "Lbl-$t" -d "$SRC" "$IMG"
    e2fsck -fn "$IMG" >/dev/null
    manifest "$SRC" "$OUT/$t.manifest"
    gzip -9 -c "$IMG" > "$OUT/$t.img.gz"
done
# Poškozený ext4 pro test „pokračovat po chybě“: z hotového obrazu se zničí jeden soubor (vynulovaný inode), jeden adresář
# (vynulovaný jediný blok s položkami) a jeden blok uprostřed velkého adresáře s HTree. Čtečka musí vypsat zbytek svazku.
DMG="$WORK/ext4-damaged.img"; cp "$WORK/ext4.img" "$DMG"
blk() { debugfs -R "blocks $1" "$DMG" 2>/dev/null | awk '{print $'"$2"'}'; }
debugfs -w -R "clri /names/Case.txt" "$DMG" >/dev/null 2>&1
debugfs -w -R "clri /big.bin" "$DMG" >/dev/null 2>&1
dd if=/dev/zero of="$DMG" bs=4096 seek="$(blk /deep/level1/level2/level3 1)" count=1 conv=notrunc 2>/dev/null
dd if=/dev/zero of="$DMG" bs=4096 seek="$(blk /many 7)" count=1 conv=notrunc 2>/dev/null
gzip -9 -c "$DMG" > "$OUT/ext4-damaged.img.gz"
ls -la "$OUT"
