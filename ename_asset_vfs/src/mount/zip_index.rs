//! Indexing a `.zip` from its central directory alone, and reading one member.
//!
//! The `zip` crate parses the central directory (Zip64 included) and nothing else. Reading and
//! inflating a member happens here, from one positioned read, so concurrent loads share no cursor.

use super::{
    Limits,
    files::read_at,
    index::{ArchiveIndex, IndexBuilder, Member, Storage},
};
use std::{
    fs::File,
    io::{self, Read},
    sync::OnceLock,
};
use zip::CompressionMethod;

const LOCAL_HEADER_LEN: usize = 30;
const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;

pub(crate) fn index(file: &File, limits: &Limits) -> Result<ArchiveIndex, String> {
    let file_len = file.metadata().map_err(|err| err.to_string())?.len();
    let archive = zip::ZipArchive::new(file).map_err(|err| err.to_string())?;
    let mut builder = IndexBuilder::new(limits);
    builder.check_count(archive.len())?;
    for i in 0..archive.len() {
        let entry = archive.by_index_data(i).map_err(|err| err.to_string())?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| format!("member {:?} is not UTF-8", entry.name_raw()))?;
        if entry.is_dir() {
            builder.add_dir(name)?;
            continue;
        }
        if entry.encrypted() {
            return Err(format!("member {name:?} is encrypted"));
        }
        if entry.is_symlink() {
            return Err(format!("member {name:?} is a symlink"));
        }
        let method = entry.compression();
        let deflated = method == CompressionMethod::DEFLATE;
        if method != CompressionMethod::STORE && !deflated {
            return Err(format!(
                "member {name:?} uses compression method {method:?}; only Stored and Deflate are supported"
            ));
        }
        let compressed_size = entry.compressed_size();
        let header_offset = entry.header_start();
        if !deflated && compressed_size != entry.size() {
            return Err(format!("stored member {name:?} has mismatched sizes"));
        }
        builder.check_size(name, compressed_size)?;
        if header_offset
            .checked_add(compressed_size)
            .is_none_or(|end| end > file_len)
        {
            return Err(format!("member {name:?} runs past the end of the file"));
        }
        builder.add_file(
            name,
            Member {
                size: entry.size(),
                storage: Storage::Zip(ZipMember {
                    header_offset,
                    compressed_size,
                    crc32: entry.crc32(),
                    deflated,
                    data_start: OnceLock::new(),
                }),
            },
        )?;
    }
    builder.finish()
}

/// Where one zip member's bytes are, from the central directory.
#[derive(Debug)]
pub(crate) struct ZipMember {
    header_offset: u64,
    compressed_size: u64,
    crc32: u32,
    deflated: bool,
    /// The data sits behind a local header whose extra field can differ in length from the
    /// central one (Info-ZIP's `UT` is 9 bytes locally, 5 centrally), so it is read on first use.
    data_start: OnceLock<u64>,
}

/// Reads and decodes one member, checking its length and CRC32.
pub(crate) fn read(file: &File, size: u64, member: &ZipMember) -> io::Result<Vec<u8>> {
    let start = match member.data_start.get() {
        Some(start) => *start,
        // A race resolves it twice, harmlessly: both reads find the same header.
        None => {
            let start = local_data_start(file, member.header_offset)?;
            *member.data_start.get_or_init(|| start)
        }
    };
    let compressed = read_at(file, start, member.compressed_size)?;
    let bytes = if member.deflated {
        // `size` was capped when indexing. One byte past it detects a member that inflates to more
        // than it declared, without ever decoding more than that.
        let mut out = Vec::with_capacity(to_usize(size)?);
        flate2::read::DeflateDecoder::new(compressed.as_slice())
            .take(size + 1)
            .read_to_end(&mut out)?;
        out
    } else {
        compressed
    };
    if bytes.len() as u64 != size {
        return Err(invalid(format!(
            "decoded {} bytes, the archive declares {size}",
            bytes.len()
        )));
    }
    if crc32fast::hash(&bytes) != member.crc32 {
        return Err(invalid("CRC32 mismatch".to_owned()));
    }
    Ok(bytes)
}

/// Where a member's data starts: after its local header, file name and local extra field.
fn local_data_start(file: &File, header_offset: u64) -> io::Result<u64> {
    let header = read_at(file, header_offset, LOCAL_HEADER_LEN as u64)?;
    let u16_at = |i: usize| u64::from(u16::from_le_bytes([header[i], header[i + 1]]));
    let signature = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if signature != LOCAL_HEADER_SIGNATURE {
        return Err(invalid(format!(
            "no local file header at offset {header_offset}"
        )));
    }
    header_offset
        .checked_add(LOCAL_HEADER_LEN as u64 + u16_at(26) + u16_at(28))
        .ok_or_else(|| invalid("local header offset overflows".to_owned()))
}

fn to_usize(size: u64) -> io::Result<usize> {
    usize::try_from(size).map_err(|_| invalid(format!("{size} bytes do not fit in memory")))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
