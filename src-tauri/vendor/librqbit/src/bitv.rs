use std::path::PathBuf;

use anyhow::Context;
use bitvec::{boxed::BitBox, order::Msb0, slice::BitSlice};

pub trait BitV: Send + Sync {
    fn is_persistent(&self) -> bool {
        false
    }
    fn as_slice(&self) -> &BitSlice<u8, Msb0>;
    fn as_slice_mut(&mut self) -> &mut BitSlice<u8, Msb0>;
    fn into_dyn(self) -> Box<dyn BitV>;
    fn as_bytes(&self) -> &[u8];
    fn flush(&mut self, flush_async: bool) -> anyhow::Result<()>;
}

pub type BoxBitV = Box<dyn BitV>;

pub struct DiskBackedBitV {
    bv: BitBox<u8, Msb0>,
    filename: PathBuf,
}

/// A complete atomic checkpoint, published only after the caller's payload
/// barrier. No drop-time flush: dropping must not persist an unsynced bitmap.
pub(crate) fn persist_snapshot(filename: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let parent = filename.parent().context("checkpoint has no parent")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(filename)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

impl DiskBackedBitV {
    pub fn new(filename: PathBuf, verified: BitBox<u8, Msb0>) -> Self {
        Self {
            bv: verified,
            filename,
        }
    }
}

#[async_trait::async_trait]
impl BitV for BitBox<u8, Msb0> {
    fn as_slice(&self) -> &BitSlice<u8, Msb0> {
        self.as_bitslice()
    }

    fn as_slice_mut(&mut self) -> &mut BitSlice<u8, Msb0> {
        self.as_mut_bitslice()
    }

    fn as_bytes(&self) -> &[u8] {
        self.as_raw_slice()
    }

    fn flush(&mut self, _flush_async: bool) -> anyhow::Result<()> {
        Ok(())
    }

    fn into_dyn(self) -> Box<dyn BitV> {
        Box::new(self)
    }
}

impl BitV for DiskBackedBitV {
    fn is_persistent(&self) -> bool {
        true
    }
    fn as_slice(&self) -> &BitSlice<u8, Msb0> {
        self.bv.as_bitslice()
    }

    fn as_slice_mut(&mut self) -> &mut BitSlice<u8, Msb0> {
        self.bv.as_mut_bitslice()
    }

    fn as_bytes(&self) -> &[u8] {
        self.bv.as_raw_slice()
    }

    fn flush(&mut self, _flush_async: bool) -> anyhow::Result<()> {
        persist_snapshot(&self.filename, self.bv.as_raw_slice())
    }

    fn into_dyn(self) -> Box<dyn BitV> {
        Box::new(self)
    }
}

impl BitV for Box<dyn BitV> {
    fn is_persistent(&self) -> bool {
        (**self).is_persistent()
    }
    fn as_slice(&self) -> &BitSlice<u8, Msb0> {
        (**self).as_slice()
    }

    fn as_slice_mut(&mut self) -> &mut BitSlice<u8, Msb0> {
        (**self).as_slice_mut()
    }

    fn as_bytes(&self) -> &[u8] {
        (**self).as_bytes()
    }

    fn flush(&mut self, flush_async: bool) -> anyhow::Result<()> {
        (**self).flush(flush_async)
    }

    fn into_dyn(self) -> Box<dyn BitV> {
        self
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    #[test]
    fn checkpoints_replace_atomically_and_drop_does_not_publish_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pieces.bitv");
        persist_snapshot(&path, &[0; 128]).unwrap();
        let mut bits =
            DiskBackedBitV::new(path.clone(), BitBox::from_boxed_slice(vec![0u8].into()));
        bits.as_slice_mut().set(0, true);
        bits.flush(false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [128]);
        bits.as_slice_mut().set(1, true);
        drop(bits);
        assert_eq!(std::fs::read(&path).unwrap(), [128]);
        // A failed publish cleans its temporary and leaves other checkpoints intact.
        let invalid = dir.path().join("directory");
        std::fs::create_dir(&invalid).unwrap();
        assert!(persist_snapshot(&invalid, &[255]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [128]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
