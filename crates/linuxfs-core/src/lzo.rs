//! Dekodér LZO1X (bezpečný Rust, bez `unsafe` a bez C knihoven) a rámování LZO, které používá btrfs.
//!
//! Formát bloku LZO1X odpovídá dekodéru `lzo1x_decompress_safe` z Linuxu (a dokumentu `Documentation/staging/lzo.rst`):
//! proud instrukcí, kde každá je buď běh doslovných bajtů, nebo shoda (délka + vzdálenost zpět ve výstupu).
//! Každý přístup ke vstupu i k výstupu je kontrolovaný, poškozená data tedy skončí chybou, nikdy ne chybou paměti.
//!
//! Rámování btrfs (`fs/btrfs/lzo.c`): komprimovaný extent začíná 32bitovou little-endian celkovou délkou
//! (včetně těchto 4 bajtů); pak následují segmenty `u32 délka` + data LZO1X, každý se rozbalí nejvýše na jeden sektor.
//! Hlavička segmentu nikdy nepřeklápí hranici sektoru: zbývají-li v sektoru méně než 4 bajty, pokračuje se
//! na začátku dalšího sektoru.

use std::fmt;

/// Chyba dekomprese LZO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LzoError {
    /// Vstup skončil uprostřed instrukce.
    InputOverrun,
    /// Výstup by přesáhl povolenou velikost.
    OutputOverrun,
    /// Shoda odkazuje před začátek výstupu (nebo má nulovou vzdálenost).
    LookBehind,
    /// Neplatná délka (příliš dlouhý řetěz nul v rozšířené délce, rámování btrfs).
    Corrupt(&'static str),
}

impl fmt::Display for LzoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LzoError::InputOverrun => f.write_str("LZO: unexpected end of the compressed data"),
            LzoError::OutputOverrun => {
                f.write_str("LZO: the data decompress to more than the allowed size")
            }
            LzoError::LookBehind => {
                f.write_str("LZO: a match points before the start of the output")
            }
            LzoError::Corrupt(m) => write!(f, "LZO: {m}"),
        }
    }
}

impl std::error::Error for LzoError {}

/// Nejvíc nul v řetězu rozšířené délky; delší řetěz je poškozený vstup (stejná mez jako v jádře).
const MAX_ZERO_RUN: usize = usize::MAX / 255 - 2;

struct Decoder<'a> {
    src: &'a [u8],
    ip: usize,
    dst: &'a mut Vec<u8>,
    /// Začátek výstupu tohoto bloku (shody smí odkazovat jen dovnitř bloku).
    base: usize,
    limit: usize,
}

impl Decoder<'_> {
    fn byte(&mut self) -> Result<usize, LzoError> {
        let b = *self.src.get(self.ip).ok_or(LzoError::InputOverrun)?;
        self.ip += 1;
        Ok(b as usize)
    }

    fn le16(&mut self) -> Result<usize, LzoError> {
        let lo = self.byte()?;
        let hi = self.byte()?;
        Ok(lo | (hi << 8))
    }

    /// Rozšířená délka: řetěz nul (každá znamená +255) zakončený nenulovým bajtem.
    fn extended(&mut self) -> Result<usize, LzoError> {
        let mut zeros = 0usize;
        loop {
            let b = self.byte()?;
            if b != 0 {
                return Ok(zeros * 255 + b);
            }
            zeros += 1;
            if zeros > MAX_ZERO_RUN {
                return Err(LzoError::Corrupt("too long run of zero length bytes"));
            }
        }
    }

    fn literals(&mut self, n: usize) -> Result<(), LzoError> {
        let end = self
            .ip
            .checked_add(n)
            .filter(|&e| e <= self.src.len())
            .ok_or(LzoError::InputOverrun)?;
        if self.dst.len() - self.base + n > self.limit {
            return Err(LzoError::OutputOverrun);
        }
        self.dst.extend_from_slice(&self.src[self.ip..end]);
        self.ip = end;
        Ok(())
    }

    fn copy_match(&mut self, dist: usize, len: usize) -> Result<(), LzoError> {
        let produced = self.dst.len() - self.base;
        if dist == 0 || dist > produced {
            return Err(LzoError::LookBehind);
        }
        if produced + len > self.limit {
            return Err(LzoError::OutputOverrun);
        }
        let from = self.dst.len() - dist;
        if dist >= len {
            self.dst.extend_from_within(from..from + len); // bez překryvu: jedno kopírování
        } else {
            for i in 0..len {
                let b = self.dst[from + i]; // překryv: bajt po bajtu (opakující se vzor)
                self.dst.push(b);
            }
        }
        Ok(())
    }
}

/// Rozbalí jeden blok LZO1X (`src`) a výsledek připojí na konec `out`. Blok smí dát nejvýše `limit` bajtů.
pub fn decompress_block(src: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<(), LzoError> {
    if src.len() < 3 {
        return Err(LzoError::InputOverrun);
    }
    let base = out.len();
    let mut d = Decoder {
        src,
        ip: 0,
        dst: out,
        base,
        limit,
    };
    // Volitelná hlavička „bitstream version" (bajt 17 + verze), kterou zná i jádro.
    if src.len() >= 5 && src[0] == 17 {
        d.ip = 2;
    }
    // stav po předchozí instrukci: 0 = nic, 1..3 = tolik doslovných bajtů na konci shody, 4 = běh doslovných bajtů
    let mut state: usize;
    if src[d.ip] > 17 {
        let t = d.byte()? - 17;
        d.literals(t)?;
        state = if t < 4 { t } else { 4 };
    } else {
        state = 0;
    }

    loop {
        if d.ip == src.len() {
            return Ok(()); // vstup skončil na hranici instrukce (bez koncové značky): shovívavě přijmout
        }
        let t = d.byte()?;
        let (len, dist, next);
        if t < 16 {
            if state == 0 {
                let mut n = t;
                if n == 0 {
                    n = d.extended()? + 15;
                }
                d.literals(n + 3)?;
                state = 4;
                continue;
            } else if state != 4 {
                // shoda délky 2 (vzdálenost do 1 KiB), za 1..3 doslovnými bajty
                let b = d.byte()?;
                d.copy_match(1 + (t >> 2) + (b << 2), 2)?;
                next = t & 3;
                d.literals(next)?;
                state = next;
                continue;
            } else {
                // shoda délky 3 se vzdáleností 2 KiB a víc, za během doslovných bajtů
                let b = d.byte()?;
                dist = 1 + 0x0800 + (t >> 2) + (b << 2);
                len = 3;
                next = t & 3;
            }
        } else if t >= 64 {
            let b = d.byte()?;
            dist = 1 + ((t >> 2) & 7) + (b << 3);
            len = (t >> 5) + 1;
            next = t & 3;
        } else if t >= 32 {
            let mut l = (t & 31) + 2;
            if l == 2 {
                l += d.extended()? + 31;
            }
            let w = d.le16()?;
            dist = 1 + (w >> 2);
            len = l;
            next = w & 3;
        } else {
            let mut l = (t & 7) + 2;
            let high = (t & 8) << 11;
            if l == 2 {
                l += d.extended()? + 7;
            }
            let w = d.le16()?;
            let off = high + (w >> 2);
            if off == 0 {
                return Ok(()); // koncová značka 0x11 0x00 0x00
            }
            dist = off + 0x4000;
            len = l;
            next = w & 3;
        }
        d.copy_match(dist, len)?;
        d.literals(next)?;
        state = next;
    }
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// Rozbalí komprimovaný extent btrfs (rámování popsané nahoře); výsledek má nejvýše `ram_bytes` bajtů.
/// Výsledek může být kratší: `ram_bytes` poslední části souboru je zaokrouhlené na celý sektor a zbytek jsou nuly
/// (doplní je volající, stejně jako jádro).
pub fn decompress_btrfs(
    src: &[u8],
    sectorsize: usize,
    ram_bytes: usize,
) -> Result<Vec<u8>, LzoError> {
    if sectorsize < 8 || !sectorsize.is_power_of_two() {
        return Err(LzoError::Corrupt("invalid sector size"));
    }
    let total = le32(src, 0).ok_or(LzoError::InputOverrun)?;
    if total > src.len() || total < 4 {
        return Err(LzoError::Corrupt(
            "the stored length does not fit the extent",
        ));
    }
    let src = &src[..total];
    let mut out = Vec::with_capacity(ram_bytes);
    let mut cur = 4usize;
    while cur < total && out.len() < ram_bytes {
        // hlavička segmentu nepřekračuje hranici sektoru
        if sectorsize - (cur % sectorsize) < 4 {
            cur = cur.next_multiple_of(sectorsize);
            if cur >= total {
                break;
            }
        }
        let seg = le32(src, cur).ok_or(LzoError::InputOverrun)?;
        cur += 4;
        let end = cur
            .checked_add(seg)
            .filter(|&e| e <= total)
            .ok_or(LzoError::InputOverrun)?;
        let want = (ram_bytes - out.len()).min(sectorsize);
        decompress_block(&src[cur..end], &mut out, want)?;
        cur = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ručně složený blok: 4 doslovné bajty „abcd", shoda délky 8 se vzdáleností 4 (M3), koncová značka.
    #[test]
    fn literal_run_and_overlapping_match() {
        // 17 + 4 = 21 = první běh 4 doslovných bajtů
        // M3: t = 32 + (8 - 2) = 38; vzdálenost 4 => w = (4 - 1) << 2 = 12 (low 2 bits = 0 doslovných za shodou)
        let src = [21u8, b'a', b'b', b'c', b'd', 38, 12, 0, 17, 0, 0];
        let mut out = Vec::new();
        decompress_block(&src, &mut out, 64).unwrap();
        assert_eq!(out, b"abcdabcdabcd");
    }

    #[test]
    fn corrupt_inputs_are_errors() {
        let mut out = Vec::new();
        assert_eq!(
            decompress_block(&[1, 2], &mut out, 10),
            Err(LzoError::InputOverrun)
        );
        // shoda před začátkem výstupu
        let src = [21u8, b'a', b'b', b'c', b'd', 38, 0x40, 0, 17, 0, 0];
        assert_eq!(
            decompress_block(&src, &mut Vec::new(), 64),
            Err(LzoError::LookBehind)
        );
        // limit výstupu
        let src = [21u8, b'a', b'b', b'c', b'd', 38, 12, 0, 17, 0, 0];
        assert_eq!(
            decompress_block(&src, &mut Vec::new(), 8),
            Err(LzoError::OutputOverrun)
        );
    }

    #[test]
    fn btrfs_framing_with_sector_padding() {
        // sektor 16 bajtů: [celková délka][délka 5][blok 5 B] skončí na bajtu 13, zbývají 3 (< 4) => další hlavička až na bajtu 16
        let seg = |c: u8| vec![18u8, c, 17, 0, 0]; // jeden doslovný bajt + koncová značka
        let mut raw = vec![0u8; 4];
        for c in [b'x', b'y', b'z'] {
            let s = seg(c);
            if 16 - (raw.len() % 16) < 4 {
                raw.resize(raw.len().next_multiple_of(16), 0); // výplň do hranice sektoru
            }
            raw.extend_from_slice(&(s.len() as u32).to_le_bytes());
            raw.extend_from_slice(&s);
        }
        let total = raw.len() as u32;
        raw[..4].copy_from_slice(&total.to_le_bytes());
        assert_eq!(decompress_btrfs(&raw, 16, 3).unwrap(), b"xyz");
        assert_eq!(decompress_btrfs(&raw, 16, 4).unwrap(), b"xyz"); // kratší než ram_bytes: zbytek jsou nuly
    }
}
