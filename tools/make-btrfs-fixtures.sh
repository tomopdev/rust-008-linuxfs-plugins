#!/usr/bin/env bash
# Vyrobí testovací obrazy btrfs pro crate linuxfs-core a manifest s obsahem, který hlásí skutečné jádro Linuxu.
#
# Spouští se JEN na Linuxu jako root (loop zařízení + mount), potřebuje btrfs-progs, python3, gzip.
# Výsledek (crates/linuxfs-core/tests/fixtures/):
#   btrfs-main.img.gz        hlavní obraz: nodesize 16 KiB, podsvazky, snapshot, komprese zstd/zlib/lzo, symlinky, ...
#   btrfs-main.manifest      co v něm je (typ, velikost, mtime, sha256, cesta) podle jádra
#   btrfs-small.img.gz       nodesize 4 KiB, hodně souborů a hluboké stromy (víceúrovňové B-stromy)
#   btrfs-small.manifest
# Testy (cargo test) pak procházejí obraz naším čtečkou a porovnávají s manifestem. Obrazy jsou deterministické
# až na UUID a časy vytvoření, proto se do repozitáře ukládají hotové a testy je nikdy negenerují znovu.
set -euo pipefail

OUT="${1:-$(dirname "$0")/../crates/linuxfs-core/tests/fixtures}"
OUT="$(cd "$OUT" && pwd)"
WORK="$(mktemp -d)"
trap 'umount "$WORK/mnt" 2>/dev/null || true; [ -n "${LOOP:-}" ] && losetup -d "$LOOP" 2>/dev/null || true; rm -rf "$WORK"' EXIT
mkdir -p "$WORK/mnt"

# Pseudonáhodná nekomprimovatelná data (opakovatelná): python, protože `openssl enc` / `shuf` nejsou všude.
rnd() { python3 - "$1" "$2" <<'PY'
import random, sys
seed, n = int(sys.argv[1]), int(sys.argv[2])
sys.stdout.buffer.write(random.Random(seed).randbytes(n))
PY
}

# Dobře komprimovatelný text (ale ne triviální opakování jednoho bajtu), délka přibližně $2 bajtů.
text() { python3 - "$1" "$2" <<'PY'
import random, sys
seed, n = int(sys.argv[1]), int(sys.argv[2])
r = random.Random(seed)
words = ["btrfs", "subvolume", "snapshot", "extent", "chunk", "leaf", "node", "zstd", "zlib", "lzo", "příliš", "žluťoučký", "kůň"]
out, size = [], 0
while size < n:
    line = " ".join(r.choice(words) for _ in range(r.randint(3, 12))) + "\n"
    out.append(line); size += len(line.encode())
sys.stdout.buffer.write("".join(out).encode()[:n])
PY
}

mount_img() { # $1 = image
    LOOP="$(losetup -f --show "$1")"
    mount "$LOOP" "$WORK/mnt"
}
umount_img() {
    sync
    umount "$WORK/mnt"
    losetup -d "$LOOP"; LOOP=""
}

manifest() { # $1 = mount, $2 = výstupní soubor: typ<TAB>velikost<TAB>mtime<TAB>sha256<TAB>cesta
    ( cd "$1"
      find . -mindepth 1 \( -type f -o -type d -o -type l \) -print0 | sort -z | while IFS= read -r -d '' p; do
          rel="${p#./}"
          if [ -L "$p" ]; then
              t=l; size=$(readlink -n "$p" | wc -c); sum=$(readlink -n "$p" | sha256sum | cut -d' ' -f1)
          elif [ -d "$p" ]; then
              t=d; size=0; sum=-
          else
              t=f; size=$(stat -c %s "$p")
              if [ "$size" -gt 1073741824 ]; then sum=-; else sum=$(sha256sum < "$p" | cut -d' ' -f1); fi
          fi
          printf '%s\t%s\t%s\t%s\t%s\n' "$t" "$size" "$(stat -c %Y "$p")" "$sum" "$rel"
      done ) > "$2"
}

# ---------------------------------------------------------------------------------------------------------------------
# 1) hlavní obraz
# ---------------------------------------------------------------------------------------------------------------------
IMG="$WORK/main.img"
truncate -s 160M "$IMG"
mkfs.btrfs -q -f -L LinuxFsMain "$IMG"
mount_img "$IMG"
M="$WORK/mnt"

# --- top-level: běžné soubory ---
printf 'Hello, btrfs!\n' > "$M/hello.txt"
: > "$M/empty.bin"
rnd 1 1500000 > "$M/big-plain.bin"
cp --reflink=always "$M/big-plain.bin" "$M/reflink.bin"
python3 - "$M" <<'PY'
import os, sys
m = sys.argv[1]
with open(m + "/sparse.bin", "wb") as f:           # díra – 4 KiB data – díra – 4 KiB data – díra
    f.seek(100000); f.write(b"A" * 4096)
    f.seek(10 * 1024 * 1024); f.write(b"B" * 4096)
    f.truncate(20 * 1024 * 1024)
with open(m + "/huge-sparse.bin", "wb") as f:       # > 4 GiB: kontrola 64bitových velikostí
    f.truncate(5 * 1024 * 1024 * 1024 + 123)
PY
fallocate -l 1M "$M/prealloc.bin"
ln "$M/hello.txt" "$M/hard1.txt"
mkdir "$M/dir"; ln "$M/hello.txt" "$M/dir/hard2.txt"
ln -s hello.txt "$M/link-rel"
ln -s /etc/passwd "$M/link-abs"
ln -s "$(python3 -c 'print("d/" * 120 + "target")')" "$M/link-long"
mkfifo "$M/fifo"
mknod "$M/chardev" c 1 3
python3 - "$M" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1] + "/sock")
PY
python3 -c 'import os,sys; os.setxattr(sys.argv[1], "user.comment", b"xattr test")' "$M/hello.txt"

# --- jména ---
mkdir "$M/names"
for n in "Příliš žluťoučký kůň.txt" "日本語.txt" "emoji-😀.txt" 'a:b?c.txt' 'CON' 'Aux.txt' 'trailing.' 'sp ace ' 'Case.txt' 'case.txt' 'CASE.TXT' 'q"uo|te<>.txt' 'back\slash.txt'; do
    printf 'name: %s\n' "$n" > "$M/names/$n"
done
printf 'x' > "$M/names/$(printf 'bad\xffutf8.bin')"             # neplatné UTF-8 (btrfs to dovolí)
touch "$M/names/$(python3 -c 'print("x" * 255)')"                 # nejdelší jméno

# --- komprese: vlastnost adresáře se dědí na nové soubory ---
for algo in zstd zlib lzo; do
    mkdir "$M/comp-$algo"
    btrfs property set "$M/comp-$algo" compression "$algo" >/dev/null
    text 10 1500000 > "$M/comp-$algo/big.txt"                   # více extentů po 128 KiB
    text 11 2000 > "$M/comp-$algo/small-inline.txt"             # vložený (inline) komprimovaný extent
    text 12 300000 > "$M/comp-$algo/medium.txt"
    rnd 13 100000 > "$M/comp-$algo/incompressible.bin"          # nekomprimovatelné zůstává nekomprimované
    # soubor přepsaný uprostřed: extenty se překrývají (extent_offset != 0)
    text 14 600000 > "$M/comp-$algo/overwritten.txt"
    python3 - "$M/comp-$algo/overwritten.txt" <<'PY'
import sys
with open(sys.argv[1], "r+b") as f:
    f.seek(150000); f.write(b"#" * 70000)
PY
done
sync

# --- velký adresář (víc listů B-stromu) ---
mkdir "$M/many"
python3 - "$M/many" <<'PY'
import os, sys
d = sys.argv[1]
for i in range(1200):
    with open(f"{d}/file-{i:05d}-" + "n" * (i % 40) + ".dat", "wb") as f:
        f.write((f"{i}\n" * (i % 300)).encode())
PY

# --- podsvazky: @, @home, vnořený, snapshot, výchozí podsvazek ---
btrfs subvolume create "$M/@" >/dev/null
btrfs subvolume create "$M/@home" >/dev/null
mkdir "$M/@/etc" "$M/@/usr"
printf 'root=@\n' > "$M/@/etc/fstab"
text 20 50000 > "$M/@/usr/data.txt"
btrfs subvolume create "$M/@/nested" >/dev/null
printf 'in nested subvolume\n' > "$M/@/nested/inner.txt"
printf 'home data\n' > "$M/@home/note.txt"
mkdir "$M/@snapshots"
btrfs subvolume snapshot -r "$M/@" "$M/@snapshots/@-snap1" >/dev/null
printf 'changed after snapshot\n' >> "$M/@/etc/fstab"
SUBID=$(btrfs subvolume list "$M" | awk '$NF=="@"{print $2}')
btrfs subvolume set-default "$SUBID" "$M" >/dev/null

# --- pevné časy (kontrola převodu časů): 2024-02-29 12:34:56 UTC ---
touch -d '2024-02-29 12:34:56 UTC' "$M/hello.txt" "$M/names/日本語.txt"
touch -h -d '2021-01-01 00:00:00 UTC' "$M/link-rel"

sync
manifest "$M" "$OUT/btrfs-main.manifest"
# podsvazky: id a jména podle jádra, pro test výpisu podsvazků
btrfs subvolume list "$M" | sed 's/^ID \([0-9]*\) gen [0-9]* top level \([0-9]*\) path /\1\t\2\t/' > "$OUT/btrfs-main.subvols"
echo "default=$SUBID" >> "$OUT/btrfs-main.subvols"
umount_img
gzip -9 -n -c "$IMG" > "$OUT/btrfs-main.img.gz"

# ---------------------------------------------------------------------------------------------------------------------
# 2) malé uzly (4 KiB): hluboké B-stromy i při málo datech
# ---------------------------------------------------------------------------------------------------------------------
IMG="$WORK/small.img"
truncate -s 160M "$IMG"
mkfs.btrfs -q -f -n 4096 -L LinuxFsSmall -m single -d single "$IMG"
mount_img "$IMG"
M="$WORK/mnt"
mkdir "$M/many"
python3 - "$M/many" <<'PY'
import sys
d = sys.argv[1]
for i in range(3000):
    with open(f"{d}/entry-{i:05d}-" + "y" * (i % 30) + ".txt", "wb") as f:
        f.write((f"line {i}\n" * (1 + i % 50)).encode())
PY
python3 - "$M" <<'PY'
import os, sys
p = sys.argv[1]
for depth in range(40):                                         # hluboký strom adresářů
    p += f"/level{depth:02d}"
    os.mkdir(p)
open(p + "/deepest.txt", "w").write("bottom\n")
PY
mount -o remount,compress-force=zstd "$M"
text 30 700000 > "$M/forced-zstd.txt"                           # jen pro tento obraz: compress-force i na malé uzly
sync
manifest "$M" "$OUT/btrfs-small.manifest"
umount_img
gzip -9 -n -c "$IMG" > "$OUT/btrfs-small.img.gz"

ls -l "$OUT"
