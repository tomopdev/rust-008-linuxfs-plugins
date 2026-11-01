//! Okno `[start, start + len)` zdroje ([`DiskSource`]) jako `BlockRead` z crate `rust-fs-core`.
//!
//! Společné pro čtečky postavené na crates od antimatter-studios (`rust-fs-squashfs`, `rust-fs-xfs`, `rust-fs-erofs`): ty čtou přes
//! tento rys, takže každé čtení prochází cache zdroje zarovnanou na sektory a nikdy nevyjde mimo oddíl.

use std::sync::{Arc, Mutex};

use fs_core::{BlockRead, Error as BlockError};

use crate::source::DiskSource;

pub(crate) struct SrcDevice {
    pub(crate) src: Arc<Mutex<DiskSource>>,
    pub(crate) start: u64,
    pub(crate) len: u64,
}

impl BlockRead for SrcDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        let end = offset.checked_add(buf.len() as u64);
        if end.is_none_or(|e| e > self.len) {
            return Err(BlockError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size: self.len,
            });
        }
        let mut src = self.src.lock().unwrap_or_else(|e| e.into_inner());
        src.read_exact_at(self.start + offset, buf).map_err(BlockError::Io)
    }

    fn size_bytes(&self) -> u64 {
        self.len
    }
}
