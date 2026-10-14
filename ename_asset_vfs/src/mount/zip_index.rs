//! Indexing a `.zip` from its central directory alone, and reading one member.
//!
//! The end-of-central-directory record is read first, so the member count and directory size are
//! bounded before the `zip` crate allocates anything for them. The crate then parses the central
//! directory (Zip64 included) and nothing else. Reading and inflating a member happens here, from
//! one positioned read.

use super::{
    Limits,
    files::read_at,
    index::{ArchiveIndex, IndexBuilder, IndexError, Member, Storage},
};
use std::{
    fs::File,
    io::{self, BufReader, Read},
    sync::OnceLock,
};
use zip::CompressionMethod;

const LOCAL_HEADER_LEN: u64 = 30;
const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const EOCD_LEN: u64 = 22;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
/// The fixed part of the Zip64 end record, signature and size field included.
const ZIP64_EOCD_LEN: u64 = 56;
/// The most extensible data accepted in a Zip64 end record.
const MAX_ZIP64_EXTENSIBLE: u64 = 64 * 1024;
/// The fixed part of one central directory record.
const CENTRAL_RECORD_LEN: u64 = 46;
/// The most central-directory bytes one member may take: the fixed record, a name at the path
/// limit, and generous room for extra fields and a comment.
const MAX_CENTRAL_RECORD: u64 = CENTRAL_RECORD_LEN + 4096 + 1024;
/// Deflate's best possible ratio is about 1032:1, so a member never decodes to more than this
/// many times its compressed size. Caps what a lying size field can make us reserve.
const MAX_DEFLATE_RATIO: u64 = 1032;

pub(crate) fn index(file: &File, limits: &Limits) -> Result<ArchiveIndex, IndexError> {
    let file_len = file.metadata()?.len();
    let mut builder = IndexBuilder::new(limits);
    let declared = declared_entries(file, file_len)?;
    builder.check_count(declared.entries)?;
    if declared.directory_size > declared.entries.saturating_mul(MAX_CENTRAL_RECORD) {
        return Err(format!(
            "a central directory of {} bytes for {} members",
            declared.directory_size, declared.entries
        )
        .into());
    }

    // Buffered: the central directory is read sequentially in small pieces.
    let archive = zip::ZipArchive::new(BufReader::with_capacity(1 << 16, file))
        .map_err(|err| err.to_string())?;
    // The crate keeps one member per name, so a shortfall means duplicate names.
    if archive.len() as u64 != declared.entries {
        return Err(format!(
            "{} members declared but {} distinct names: duplicate member names",
            declared.entries,
            archive.len()
        )
        .into());
    }

    // Every local header must sit clear of the data before it, or members overlap (the
    // overlapping-file zip bomb). Each member's data must end before the next header starts.
    let mut headers: Vec<u64> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        headers.push(
            archive
                .by_index_data(i)
                .map_err(|e| e.to_string())?
                .header_start(),
        );
    }
    headers.sort_unstable();
    if headers.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("two members share one local header".to_owned().into());
    }
    let directory_start = archive.central_directory_start();
    let next_header = |offset: u64| {
        let i = headers.partition_point(|&header| header <= offset);
        headers.get(i).copied().unwrap_or(directory_start)
    };

    for i in 0..archive.len() {
        let entry = archive.by_index_data(i).map_err(|err| err.to_string())?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| format!("member {:?} is not UTF-8", entry.name_raw()))?;
        let header_offset = entry.header_start();
        let compressed_size = entry.compressed_size();
        let data_limit = next_header(header_offset);
        if header_offset
            .checked_add(LOCAL_HEADER_LEN)
            .and_then(|end| end.checked_add(compressed_size))
            .is_none_or(|end| end > data_limit || end > file_len)
        {
            return Err(format!("member {name:?} overlaps the next member").into());
        }
        if entry.is_dir() {
            builder.add_dir(name)?;
            continue;
        }
        if entry.encrypted() {
            return Err(format!("member {name:?} is encrypted").into());
        }
        if entry.is_symlink() {
            return Err(format!("member {name:?} is a symlink").into());
        }
        let method = entry.compression();
        let deflated = method == CompressionMethod::DEFLATE;
        if method != CompressionMethod::STORE && !deflated {
            return Err(format!(
                "member {name:?} uses compression method {method:?}; only Stored and Deflate are \
                 supported"
            )
            .into());
        }
        if !deflated && compressed_size != entry.size() {
            return Err(format!("stored member {name:?} has mismatched sizes").into());
        }
        builder.check_size(name, compressed_size)?;
        builder.add_file(
            name,
            Member {
                size: entry.size(),
                storage: Storage::Zip(ZipMember {
                    header_offset,
                    compressed_size,
                    data_limit,
                    crc32: entry.crc32(),
                    deflated,
                    data_start: OnceLock::new(),
                }),
            },
        )?;
    }
    Ok(builder.finish()?)
}

struct Declared {
    entries: u64,
    directory_size: u64,
}

/// The member count and central directory size the end-of-central-directory record declares,
/// from its Zip64 version when a Zip64 locator precedes it, as the `zip` crate reads it.
fn declared_entries(file: &File, file_len: u64) -> Result<Declared, IndexError> {
    let tail_len = file_len.min(EOCD_LEN + u64::from(u16::MAX));
    let tail_start = file_len - tail_len;
    let tail = read_at(file, tail_start, tail_len)?;
    // The record whose comment ends exactly at the end of the file. A comment may itself
    // contain the signature.
    let at = (0..tail.len().saturating_sub(EOCD_LEN as usize - 1))
        .rev()
        .find(|&at| {
            tail[at..at + 4] == EOCD_SIGNATURE
                && at + EOCD_LEN as usize + u16_at(&tail, at + 20) as usize == tail.len()
        })
        .ok_or_else(|| "no end of central directory record".to_owned())?;
    let eocd = &tail[at..];
    let eocd_offset = tail_start + at as u64;
    let locator = match eocd_offset.checked_sub(20) {
        Some(offset) => Some(read_at(file, offset, 20)?)
            .filter(|locator| u32_at(locator, 0) == u64::from(ZIP64_LOCATOR_SIGNATURE)),
        None => None,
    };
    let declared = match locator {
        None => {
            if u32_at(eocd, 12) == u64::from(u32::MAX) || u32_at(eocd, 16) == u64::from(u32::MAX) {
                return Err("Zip64 sizes without a Zip64 locator".to_owned().into());
            }
            check_disk_counts(u16_at(eocd, 8), u16_at(eocd, 10))?;
            Declared {
                entries: u16_at(eocd, 10),
                directory_size: u32_at(eocd, 12),
            }
        }
        Some(locator) => {
            let record_offset = u64_at(&locator, 8);
            if record_offset
                .checked_add(ZIP64_EOCD_LEN)
                .is_none_or(|end| end > eocd_offset - 20)
            {
                return Err("the Zip64 record lies outside the file".to_owned().into());
            }
            let record = read_at(file, record_offset, ZIP64_EOCD_LEN)?;
            if u32_at(&record, 0) != u64::from(ZIP64_EOCD_SIGNATURE) {
                return Err("no Zip64 end of central directory record".to_owned().into());
            }
            // The crate reads the record's extensible data, sized by this field, into memory.
            let record_size = u64_at(&record, 4);
            if !(ZIP64_EOCD_LEN - 12..=ZIP64_EOCD_LEN - 12 + MAX_ZIP64_EXTENSIBLE)
                .contains(&record_size)
                || record_offset + 12 + record_size > eocd_offset - 20
            {
                return Err(format!("a Zip64 record of {record_size} bytes").into());
            }
            check_disk_counts(u64_at(&record, 24), u64_at(&record, 32))?;
            Declared {
                entries: u64_at(&record, 32),
                directory_size: u64_at(&record, 40),
            }
        }
    };
    if declared.directory_size < declared.entries.saturating_mul(CENTRAL_RECORD_LEN)
        || declared.directory_size > file_len
    {
        return Err(format!(
            "a central directory of {} bytes cannot hold {} members",
            declared.directory_size, declared.entries
        )
        .into());
    }
    Ok(declared)
}

/// The crate counts the members on this disk; the limits use the total. Only single-disk archives
/// are supported, so the two must agree.
fn check_disk_counts(on_disk: u64, total: u64) -> Result<(), String> {
    if on_disk != total {
        return Err(format!(
            "{total} members in total but {on_disk} on this disk; multi-disk archives are not \
             supported"
        ));
    }
    Ok(())
}

fn u16_at(bytes: &[u8], i: usize) -> u64 {
    u64::from(u16::from_le_bytes([bytes[i], bytes[i + 1]]))
}

fn u32_at(bytes: &[u8], i: usize) -> u64 {
    u64::from(u32::from_le_bytes(
        bytes[i..i + 4].try_into().expect("four bytes"),
    ))
}

fn u64_at(bytes: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(bytes[i..i + 8].try_into().expect("eight bytes"))
}

/// Where one zip member's bytes are, from the central directory.
#[derive(Debug)]
pub(crate) struct ZipMember {
    header_offset: u64,
    compressed_size: u64,
    /// Where the next member's local header (or the central directory) starts. The data must end
    /// before it.
    data_limit: u64,
    crc32: u32,
    deflated: bool,
    /// The data sits behind a local header whose extra field can differ in length from the
    /// central one (Info-ZIP's `UT` is 9 bytes locally, 5 centrally), so it is read on first use.
    data_start: OnceLock<u64>,
}

/// Reads and decodes one member, checking its bounds, length and CRC32.
pub(crate) fn read(file: &File, size: u64, member: &ZipMember) -> io::Result<Vec<u8>> {
    let start = match member.data_start.get() {
        Some(start) => *start,
        // A race resolves it twice, harmlessly: both reads find the same header.
        None => {
            let start = local_data_start(file, member.header_offset)?;
            *member.data_start.get_or_init(|| start)
        }
    };
    if start
        .checked_add(member.compressed_size)
        .is_none_or(|end| end > member.data_limit)
    {
        return Err(invalid("member data overlaps the next member".to_owned()));
    }
    let compressed = read_at(file, start, member.compressed_size)?;
    let bytes = if member.deflated {
        // `size` was capped when indexing. One byte past it detects a member that inflates to more
        // than it declared, without ever decoding more than that.
        let reserve = size.min(member.compressed_size.saturating_mul(MAX_DEFLATE_RATIO));
        let mut out = Vec::with_capacity(to_usize(reserve)?);
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
    let header = read_at(file, header_offset, LOCAL_HEADER_LEN)?;
    let u16_at = |i: usize| u64::from(u16::from_le_bytes([header[i], header[i + 1]]));
    let signature = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if signature != LOCAL_HEADER_SIGNATURE {
        return Err(invalid(format!(
            "no local file header at offset {header_offset}"
        )));
    }
    header_offset
        .checked_add(LOCAL_HEADER_LEN + u16_at(26) + u16_at(28))
        .ok_or_else(|| invalid("local header offset overflows".to_owned()))
}

fn to_usize(size: u64) -> io::Result<usize> {
    usize::try_from(size).map_err(|_| invalid(format!("{size} bytes do not fit in memory")))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
