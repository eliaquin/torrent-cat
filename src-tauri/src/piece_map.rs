use anyhow::{Context, Result};
use librqbit::api::TorrentIdOrHash;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePieces {
    pub file_length: u64,
    pub downloaded_bytes: u64,
    pub ranges: Vec<[u64; 2]>,
}

impl crate::core::Core {
    pub fn file_pieces(&self, id: usize, index: usize) -> Result<Option<FilePieces>> {
        let id = TorrentIdOrHash::Id(id);
        let handle = self.api.mgr_handle(id)?;
        let metadata = handle
            .metadata
            .load_full()
            .context("Torrent metadata unavailable")?;
        let file = metadata.file_infos.get(index).context("File not found")?;
        // While checking files, availability is unknown rather than a solid prefix.
        let Ok((have, _)) = self.api.api_dump_haves(id) else {
            return Ok(None);
        };
        Ok(Some(file_ranges(
            file.offset_in_torrent,
            file.len,
            u64::from(metadata.lengths().default_piece_length()),
            |piece| have.get(piece).is_some_and(|bit| *bit),
        )))
    }
}

// Clip verified torrent pieces to this file, including pieces shared with neighbors.
fn file_ranges(
    offset: u64,
    length: u64,
    piece_length: u64,
    mut have: impl FnMut(usize) -> bool,
) -> FilePieces {
    let mut result = FilePieces {
        file_length: length,
        downloaded_bytes: 0,
        ranges: vec![],
    };
    if length == 0 {
        return result;
    }
    let end = offset + length;
    for piece in offset / piece_length..end.div_ceil(piece_length) {
        if !have(piece as usize) {
            continue;
        }
        let start = (piece * piece_length).max(offset) - offset;
        let stop = ((piece + 1) * piece_length).min(end) - offset;
        result.downloaded_bytes += stop - start;
        if let Some(last) = result.ranges.last_mut().filter(|last| last[1] == start) {
            last[1] = stop;
        } else {
            result.ranges.push([start, stop]);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_pieces_preserve_gaps_and_merge_neighbors() {
        let map = file_ranges(0, 65, 10, |i| [0, 2, 3, 6].contains(&i));
        assert_eq!(map.ranges, vec![[0, 10], [20, 40], [60, 65]]);
        assert_eq!(map.downloaded_bytes, 35);
    }

    #[test]
    fn file_boundaries_clip_shared_pieces_and_exclude_other_files() {
        let map = file_ranges(15, 38, 10, |i| [0, 1, 3, 5, 6].contains(&i));
        assert_eq!(map.ranges, vec![[0, 5], [15, 25], [35, 38]]);
        assert_eq!(map.downloaded_bytes, 18);
        assert_eq!(map.file_length, 38);
    }

    #[test]
    fn empty_missing_and_complete_files() {
        assert!(file_ranges(5, 0, 10, |_| true).ranges.is_empty());
        assert!(file_ranges(5, 25, 10, |_| false).ranges.is_empty());
        let map = file_ranges(5, 25, 10, |_| true);
        assert_eq!(map.ranges, vec![[0, 25]]);
        assert_eq!(map.downloaded_bytes, 25);
    }
}
