//! Indexing a plain `.tar` by reading each header and seeking past the data.
//!
//! Tar has no payload checksum, so a member read is just its bytes. Archives are treated as
//! immutable while the game runs.

use super::{
    Limits,
    index::{ArchiveIndex, IndexBuilder, Member, Storage},
};
use std::{fs::File, io::BufReader};
use tar::EntryType;

pub(crate) fn index(file: &File, limits: &Limits) -> Result<ArchiveIndex, String> {
    let file_len = file.metadata().map_err(|err| err.to_string())?.len();
    // `entries_with_seek` reads each 512-byte header and seeks over the data. `entries` would read
    // the whole archive.
    let mut archive = tar::Archive::new(BufReader::new(file));
    let mut builder = IndexBuilder::new(limits);
    for entry in archive.entries_with_seek().map_err(|err| err.to_string())? {
        let mut entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path_bytes();
        let name = std::str::from_utf8(&path)
            .map_err(|_| format!("member {path:?} is not UTF-8"))?
            .to_owned();
        match entry.header().entry_type() {
            EntryType::Directory => {
                builder.add_dir(&name)?;
                continue;
            }
            EntryType::Regular | EntryType::Continuous => {}
            // Metadata for the entries after it, which the `tar` crate has already applied.
            EntryType::XGlobalHeader => continue,
            other => return Err(format!("member {name:?} is a {other:?} entry")),
        }
        let sparse = entry
            .pax_extensions()
            .map_err(|err| err.to_string())?
            .into_iter()
            .flatten()
            .flatten()
            .any(|ext| ext.key_bytes().starts_with(b"GNU.sparse"));
        if sparse {
            return Err(format!("member {name:?} is a sparse file"));
        }
        let offset = entry.raw_file_position();
        let size = entry.size();
        if offset.checked_add(size).is_none_or(|end| end > file_len) {
            return Err(format!("member {name:?} runs past the end of the file"));
        }
        builder.add_file(
            &name,
            Member {
                size,
                storage: Storage::Tar { offset },
            },
        )?;
    }
    builder.finish()
}
