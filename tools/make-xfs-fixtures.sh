#!/usr/bin/env bash
# Vyrobí testovací obrazy XFS pro crate linuxfs-core a manifesty s obsahem, který hlásí skutečné jádro Linuxu.
#
# Spouští se JEN na Linuxu jako root (loop zařízení + mount), potřebuje xfsprogs (mkfs.xfs, xfs_repair, xfs_db, xfs_io,
# xfs_bmap), python3, gzip a jádro s XFS (v5; v4 jádra od 6.18 ve výchozím nastavení nemountují, proto se v4 obraz plní
# přes `mkfs.xfs -p` a nepotřebuje mount). mkfs.xfs chce pro v5 aspoň 300 MB, obrazy jsou řídké a po gzipu malé.
# Výsledek (crates/linuxfs-core/tests/fixtures/):
#   xfs-v5.img.gz          4 KiB bloky, inody 512 B, crc + finobt + reflink + rmapbt + bigtime + inobtcount (výchozí v mkfs.xfs 6.x)
#   xfs-v5.manifest        co v něm je (typ, velikost, mtime, sha256, cesta) podle jádra
#   xfs-v5-small.img.gz    1 KiB bloky, adresářový blok 4 KiB (víc bloků fs), 4 alokační skupiny, žurnál 64 MB, bez reflink / rmapbt / bigtime / inobtcount
#   xfs-v5-small.manifest
#   xfs-v4.img.gz          v4 (bez crc), 2 KiB bloky, inody 256 B, adresáře bez typu souboru (ftype=0); plní se přes `mkfs.xfs -p`,
#   xfs-v4.manifest        proto čas změny není z jádra, ale z doby výroby: v manifestu je mtime `-` a test ho nekontroluje
#   xfs-dirty.img.gz       v5 s NEČISTÝM žurnálem: po zásahu `shutdown` zůstaly změny jen v žurnálu (přejmenování, nový adresář,
#   xfs-dirty.manifest     smazaný soubor); manifest je stav po obnově žurnálu jádrem (obraz se na to připojí jako kopie)
#   xfs-damaged.img.gz     kopie xfs-v5 se zničenými inody (/big.bin, /names/Case.txt, /deep/level1/level2/level3) a prvním
#                          blokem adresáře /many; čtečka musí vypsat zbytek svazku a poškození ohlásit
# Testy (cargo test) pak procházejí obraz naší čtečkou a porovnávají s manifestem. Obrazy jsou deterministické až na UUID
# a časy, proto se do repozitáře ukládají hotové a testy je nikdy negenerují znovu.
set -euo pipefail

OUT="${1:-$(dirname "$0")/../crates/linuxfs-core/tests/fixtures}"
OUT="$(cd "$OUT" && pwd)"
WORK="$(mktemp -d)"
LOOP=""
trap 'umount "$WORK/mnt" 2>/dev/null || true; [ -n "$LOOP" ] && losetup -d "$LOOP" 2>/dev/null || true; rm -rf "$WORK"' EXIT
mkdir -p "$WORK/mnt"
M="$WORK/mnt"

rnd() { python3 - "$1" "$2" <<'PY'
import random, sys
sys.stdout.buffer.write(random.Random(int(sys.argv[1])).randbytes(int(sys.argv[2])))
PY
}

# $1 = adresář, $2 = bigtime (1 = i čas po roce 2038), $3 = reflink (1 = kopie sdílející extenty),
# $4 = proto (1 = bez jmen s mezerami a bez hard linku, protože prototypový soubor mkfs.xfs je neumí)
populate() {
    local D="$1" bigtime="$2" reflink="$3" proto="$4"
    printf 'Hello, xfs!\n' > "$D/hello.txt"
    : > "$D/empty.bin"
    printf 'x' > "$D/tiny.txt"
    rnd 1 1500000 > "$D/big.bin"
    python3 - "$D" <<'PY'
import sys
m = sys.argv[1]
with open(m + "/sparse.bin", "wb") as f:                # díry mezi daty (nealokované úseky čtou jako nuly)
    f.seek(100000); f.write(b"A" * 4096)
    f.seek(10 * 1024 * 1024); f.write(b"B" * 4096)
    f.truncate(20 * 1024 * 1024)
# silně roztříštěný soubor: každý druhý blok je díra, takže každý zapsaný blok je vlastní extent (na 4 KiB blocích
# přes 2000 záznamů = mapa extentů už nevejde do inodu a je z ní B+strom, tzv. bmbt)
with open(m + "/fragmented.bin", "wb") as f:
    for i in range(1, 2501):
        f.seek(i * 8192); f.write((b"%06d" % i) * 683 + b"\n" * 2)   # 4098 B: zasahuje i do druhého bloku
PY
    fallocate -l 1048576 "$D/prealloc.bin"                # předalokované (unwritten) extenty se čtou jako nuly
    head -c 4096 /dev/zero | tr '\0' Z | dd of="$D/prealloc.bin" conv=notrunc 2>/dev/null
    if [ "$proto" = 1 ]; then cp "$D/hello.txt" "$D/hard1.txt"; else ln "$D/hello.txt" "$D/hard1.txt"; fi
    ln -s hello.txt "$D/link-rel"
    ln -s /etc/passwd "$D/link-abs"
    ln -s "$(python3 -c 'print("d/" * 60 + "target")')" "$D/link-long"      # 126 B: v inodu 512 B uvnitř, v inodu 256 B (v4) v bloku
    ln -s "$(python3 -c 'print("e/" * 200 + "target")')" "$D/link-xlong"    # 406 B: vždy v samostatném bloku (s hlavičkou na v5)
    mkfifo "$D/fifo"
    mknod "$D/chardev" c 1 3 2>/dev/null || true
    mkdir "$D/names"
    local spaced="Příliš žluťoučký kůň.txt"
    [ "$proto" = 1 ] && spaced="Příliš_žluťoučký_kůň.txt"
    for n in "$spaced" "日本語.txt" 'a:b?c.txt' 'CON' 'trailing.' 'Case.txt' 'case.txt'; do
        printf 'name: %s\n' "$n" > "$D/names/$n"
    done
    printf 'x' > "$D/names/$(printf 'bad\xffutf8.bin')"
    touch "$D/names/$(python3 -c 'print("x" * 255)')"
    mkdir "$D/many"                                        # tisíce položek: adresář ve tvaru leaf / node přes mnoho bloků
    for i in $(seq 1 3000); do printf '%s' "$i" > "$D/many/file-with-a-longer-name-$i.txt"; done
    mkdir "$D/few"                                         # pár položek: krátký tvar adresáře přímo v inodu
    for i in 1 2 3; do printf '%s' "$i" > "$D/few/f$i"; done
    local d="$D/deep"; mkdir "$d"
    for i in $(seq 1 30); do d="$d/level$i"; mkdir "$d"; done
    printf 'bottom\n' > "$d/leaf.txt"
    printf 'old\n' > "$D/old-1950.txt";  touch -d '1950-01-01 00:00:00 UTC' "$D/old-1950.txt"    # před rokem 1970
    if [ "$bigtime" = 1 ]; then
        printf 'future\n' > "$D/future-2100.txt"; touch -d '2100-01-01 00:00:00 UTC' "$D/future-2100.txt"   # za rokem 2038
    fi
    if [ "$reflink" = 1 ]; then
        cp --reflink=always "$D/big.bin" "$D/reflink.bin"
        head -c 4096 /dev/zero | tr '\0' R | dd of="$D/reflink.bin" bs=4096 seek=100 conv=notrunc 2>/dev/null   # část extentů přestane být sdílená
    fi
}

manifest() { # $1 = adresář (mount nebo zdroj), $2 = výstup, $3 = "-" pokud se mtime nemá zapsat
    ( cd "$1"
      find . -mindepth 1 \( -type f -o -type d -o -type l \) -print0 | sort -z | while IFS= read -r -d '' p; do
          rel="${p#./}"
          if [ -L "$p" ]; then t=l; size=$(readlink -n "$p" | wc -c); sum=$(readlink -n "$p" | sha256sum | cut -d' ' -f1)
          elif [ -d "$p" ]; then t=d; size=0; sum=-
          else t=f; size=$(stat -c %s "$p"); sum=$(sha256sum < "$p" | cut -d' ' -f1); fi
          mt=$(stat -c %Y "$p"); [ "${3:-}" = "-" ] && mt=-
          printf '%s\t%s\t%s\t%s\t%s\n' "$t" "$size" "$mt" "$sum" "$rel"
      done ) > "$2"
}

mount_img() { LOOP="$(losetup -f --show "$1")"; mount -o noatime "$LOOP" "$M"; }
umount_img() { sync; umount "$M"; losetup -d "$LOOP"; LOOP=""; }
check_img() { xfs_repair -n "$1" > "$WORK/repair.log" 2>&1 || { cat "$WORK/repair.log" >&2; echo "xfs_repair -n: $1 není konzistentní" >&2; exit 1; }; }

# $1 = jméno, $2 = bigtime, $3 = reflink, zbytek = přepínače mkfs.xfs
make_mounted() {
    local name="$1" bigtime="$2" reflink="$3"; shift 3
    local img="$WORK/$name.img"
    truncate -s 320M "$img"
    mkfs.xfs -q -f "$@" "$img"
    mount_img "$img"
    populate "$M" "$bigtime" "$reflink" 0
    manifest "$M" "$OUT/$name.manifest"
    # inody pro poškozený obraz si zapamatujeme, dokud je svazek připojený
    if [ "$name" = xfs-v5 ]; then
        {   stat -c 'big %i' "$M/big.bin"; stat -c 'case %i' "$M/names/Case.txt"; stat -c 'level3 %i' "$M/deep/level1/level2/level3"
            stat -c 'many %i' "$M/many"; xfs_bmap -v "$M/many" | awk '/^ *0:/ {split($3, a, "\\.\\."); print "manyblk " a[1]}'
        } > "$WORK/damage.info"
    fi
    umount_img
    check_img "$img"     # svazek musí být konzistentní
    gzip -9 -c "$img" > "$OUT/$name.img.gz"
    echo "$name: $(stat -c %s "$OUT/$name.img.gz") B (gz)"
}

make_mounted xfs-v5 1 1 -b size=4096 -m crc=1,finobt=1,reflink=1,rmapbt=1,inobtcount=1,bigtime=1 -L LinuxFsV5
make_mounted xfs-v5-small 0 0 -b size=1024 -s size=512 -n size=4096 -d agcount=4 -l size=64m \
    -m crc=1,finobt=1,reflink=0,rmapbt=0,inobtcount=0,bigtime=0 -L LinuxFsV5Sm

# ---------------------------------------------------------------------------------------------------------------------
# v4: bez mountu, plní se prototypovým souborem (mkfs.xfs -p). Jména bez mezer, bez hard linku (viz populate).
# ---------------------------------------------------------------------------------------------------------------------
SRC="$WORK/src4"; mkdir "$SRC"; populate "$SRC" 0 0 1
python3 - "$SRC" "$WORK/proto" <<'PY'
import os, stat, sys
root, out = sys.argv[1], sys.argv[2]
lines = ["/dev/null", "0 0", "d--755 0 0"]
def emit(path, depth):
    ind = " " * depth
    for name in sorted(os.listdir(path)):
        p = os.path.join(path, name); st = os.lstat(p); mode = "%03o" % (st.st_mode & 0o777)
        if stat.S_ISDIR(st.st_mode):
            lines.append(f"{ind}{name} d--{mode} 0 0"); emit(p, depth + 1); lines.append(f"{ind}$")
        elif stat.S_ISLNK(st.st_mode):
            lines.append(f"{ind}{name} l--{mode} 0 0 {os.readlink(p)}")
        elif stat.S_ISFIFO(st.st_mode):
            lines.append(f"{ind}{name} p--{mode} 0 0")
        elif stat.S_ISCHR(st.st_mode):
            lines.append(f"{ind}{name} c--{mode} 0 0 {os.major(st.st_rdev)} {os.minor(st.st_rdev)}")
        elif stat.S_ISREG(st.st_mode):
            lines.append(f"{ind}{name} ---{mode} 0 0 {p}")
emit(root, 1)
lines.append("$")
open(out, "w", encoding="utf-8", errors="surrogateescape").write("\n".join(lines) + "\n")
PY
IMG="$WORK/xfs-v4.img"; truncate -s 320M "$IMG"
mkfs.xfs -q -f -b size=2048 -i size=256 -m crc=0,finobt=0 -L LinuxFsV4 -p "$WORK/proto" "$IMG" 2>/dev/null
check_img "$IMG"
manifest "$SRC" "$OUT/xfs-v4.manifest" -
gzip -9 -c "$IMG" > "$OUT/xfs-v4.img.gz"
echo "xfs-v4: $(stat -c %s "$OUT/xfs-v4.img.gz") B (gz)"

# ---------------------------------------------------------------------------------------------------------------------
# nečistý žurnál: na hotovém svazku se pár věcí změní a pak se vynutí `shutdown` (jako pád): žurnál se zapíše, metadata
# na disk ne. Obraz se uloží před obnovou; manifest se vezme z kopie, kterou jádro po připojení obnoví.
# ---------------------------------------------------------------------------------------------------------------------
IMG="$WORK/xfs-dirty.img"; truncate -s 320M "$IMG"
mkfs.xfs -q -f -b size=4096 -m crc=1,reflink=1,rmapbt=1,bigtime=1 -L LinuxFsDirty "$IMG"
mount_img "$IMG"; populate "$M" 1 1 0; umount_img
mount_img "$IMG"
mv "$M/hello.txt" "$M/renamed.txt"
rm "$M/empty.bin" "$M/names/case.txt"
mkdir "$M/late" "$M/late/sub"
for i in $(seq 1 40); do printf 'late file %s\n' "$i" > "$M/late/f$i.txt"; done
printf 'data on disk\n' > "$M/late/sub/synced.txt"
ln -s ../renamed.txt "$M/late/link-to-renamed"
xfs_io -c fsync "$M/late/sub/synced.txt"
xfs_io -c fsync "$M/renamed.txt"
xfs_io -x -c 'shutdown -f' "$M"
umount_img
xfs_repair -n "$IMG" > "$WORK/repair.log" 2>&1 || true      # výstup do souboru: `grep -q` v rouře by s pipefail shodil skript (SIGPIPE)
if ! grep -q 'valuable metadata changes in a log' "$WORK/repair.log"; then
    echo "obraz nemá nečistý žurnál (shutdown nic nezanechal)" >&2; exit 1
fi
cp --sparse=always "$IMG" "$WORK/xfs-dirty-replayed.img"
gzip -9 -c "$IMG" > "$OUT/xfs-dirty.img.gz"
mount_img "$WORK/xfs-dirty-replayed.img"      # jádro žurnál přehraje
manifest "$M" "$OUT/xfs-dirty.manifest"
umount_img
echo "xfs-dirty: $(stat -c %s "$OUT/xfs-dirty.img.gz") B (gz)"

# ---------------------------------------------------------------------------------------------------------------------
# poškozený svazek: z čistého xfs-v5 se zničí tři inody (magie `IN` přepsaná nulami) a první datový blok velkého adresáře
# /many (vynulovaný celý blok).
# ---------------------------------------------------------------------------------------------------------------------
DMG="$WORK/xfs-damaged.img"; cp --sparse=always "$WORK/xfs-v5.img" "$DMG"
get() { awk -v k="$1" '$1 == k {print $2}' "$WORK/damage.info"; }
python3 - "$DMG" "$(get big)" "$(get case)" "$(get level3)" <<'PY'
# xfs_db odmítne zapsat vadný inod (write verifier), proto se magie `IN` přepíše přímo v obrazu: vzorec z čísla inodu
# (alokační skupina | blok ve skupině | pořadí v bloku), CRC zůstane původní, takže inod neprojde ani kontrolou magie, ani součtu
import struct, sys
img = open(sys.argv[1], "r+b")
sb = img.read(512)
blocksize, agblocks = struct.unpack_from(">I", sb, 4)[0], struct.unpack_from(">I", sb, 84)[0]
inodesize = struct.unpack_from(">H", sb, 104)[0]
inopblog, agblklog = sb[123], sb[124]
for ino in map(int, sys.argv[2:]):
    agno = ino >> (agblklog + inopblog)
    agino = ino & ((1 << (agblklog + inopblog)) - 1)
    off = (agno * agblocks + (agino >> inopblog)) * blocksize + (agino & ((1 << inopblog) - 1)) * inodesize
    img.seek(off); assert img.read(2) == b"IN", (ino, off)
    img.seek(off); img.write(b"\0\0")
img.close()
PY
dd if=/dev/zero of="$DMG" bs=512 seek="$(get manyblk)" count=8 conv=notrunc 2>/dev/null     # 8 x 512 = jeden blok 4 KiB
gzip -9 -c "$DMG" > "$OUT/xfs-damaged.img.gz"
echo "xfs-damaged: $(stat -c %s "$OUT/xfs-damaged.img.gz") B (gz)"
ls -la "$OUT" | grep -i xfs
