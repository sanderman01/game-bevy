//! Indexing a `.zip` from its central directory alone, and reading one member.
//!
//! Parsed here rather than with the `zip` crate: the crate picks its own end record, falling back
//! to earlier candidates, and allocates for whatever it finds before any limit can apply. Here the
//! one end record whose comment reaches the end of the file is validated first, and exactly the
//! central directory it bounds is read. Zip64 sizes and offsets are understood; archives with data
//! prepended (self-extracting) or spanning several disks are not. Reading and inflating a member
//! happens from one positioned read.

use super::{
    Limits,
    files::read_at,
    index::{ArchiveIndex, IndexBuilder, IndexError, Member, Storage},
};
use std::{
    fs::File,
    io::{self, Read},
    sync::OnceLock,
};

const LOCAL_HEADER_LEN: u64 = 30;
const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u64 = 0x0201_4b50;
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
const STORED: u64 = 0;
const DEFLATED: u64 = 8;
const ZIP64_EXTRA: u64 = 0x0001;
/// General purpose flags: encrypted, and strongly encrypted.
const ENCRYPTED_FLAGS: u64 = 0x0001 | 0x0040;
const UNIX_HOST: u64 = 3;
const UNIX_TYPE_MASK: u64 = 0o170_000;
const UNIX_SYMLINK: u64 = 0o120_000;

/// One central directory record, sizes and offset already widened by its Zip64 extra field.
struct Central<'a> {
    name: &'a [u8],
    flags: u64,
    method: u64,
    crc32: u32,
    compressed_size: u64,
    size: u64,
    header_offset: u64,
    symlink: bool,
}

pub(crate) fn index(file: &File, limits: &Limits) -> Result<ArchiveIndex, IndexError> {
    let file_len = file.metadata()?.len();
    let mut builder = IndexBuilder::new(limits);
    let footer = footer(file, file_len)?;
    builder.check_count(footer.entries)?;
    if footer.directory_size > footer.entries.saturating_mul(MAX_CENTRAL_RECORD) {
        return Err(format!(
            "a central directory of {} bytes for {} members",
            footer.directory_size, footer.entries
        )
        .into());
    }
    let directory = read_at(file, footer.directory_offset, footer.directory_size)?;
    let records = parse_directory(&directory, footer.entries)?;

    // Every local header must sit clear of the data before it, or members overlap (the
    // overlapping-file zip bomb). Each member's data must end before the next header starts.
    let mut headers: Vec<u64> = records.iter().map(|record| record.header_offset).collect();
    headers.sort_unstable();
    if headers.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("two members share one local header".to_owned().into());
    }
    let next_header = |offset: u64| {
        let i = headers.partition_point(|&header| header <= offset);
        headers.get(i).copied().unwrap_or(footer.directory_offset)
    };

    for record in records {
        let name = std::str::from_utf8(record.name)
            .map_err(|_| format!("member {:?} is not UTF-8", record.name))?;
        let data_limit = next_header(record.header_offset);
        if record
            .header_offset
            .checked_add(LOCAL_HEADER_LEN)
            .and_then(|end| end.checked_add(record.compressed_size))
            .is_none_or(|end| end > data_limit)
        {
            return Err(format!("member {name:?} overlaps the next member").into());
        }
        if name.ends_with('/') {
            builder.add_dir(name)?;
            continue;
        }
        if record.flags & ENCRYPTED_FLAGS != 0 {
            return Err(format!("member {name:?} is encrypted").into());
        }
        if record.symlink {
            return Err(format!("member {name:?} is a symlink").into());
        }
        let deflated = match record.method {
            STORED => false,
            DEFLATED => true,
            method => {
                return Err(format!(
                    "member {name:?} uses compression method {method}; only Stored and Deflate \
                     are supported"
                )
                .into());
            }
        };
        if !deflated && record.compressed_size != record.size {
            return Err(format!("stored member {name:?} has mismatched sizes").into());
        }
        builder.check_size(name, record.compressed_size)?;
        builder.add_file(
            name,
            Member {
                size: record.size,
                storage: Storage::Zip(ZipMember {
                    header_offset: record.header_offset,
                    compressed_size: record.compressed_size,
                    data_limit,
                    crc32: record.crc32,
                    deflated,
                    data_start: OnceLock::new(),
                }),
            },
        )?;
    }
    Ok(builder.finish()?)
}

/// Exactly `entries` records, filling `directory` exactly.
fn parse_directory(directory: &[u8], entries: u64) -> Result<Vec<Central<'_>>, String> {
    let bad = |at: usize| format!("malformed central directory record at byte {at}");
    let mut records = Vec::with_capacity(usize::try_from(entries).map_err(|_| bad(0))?);
    let mut at = 0;
    for _ in 0..entries {
        let fixed = directory
            .get(at..at + CENTRAL_RECORD_LEN as usize)
            .ok_or_else(|| bad(at))?;
        if u32_at(fixed, 0) != CENTRAL_SIGNATURE {
            return Err(bad(at));
        }
        let name_len = u16_at(fixed, 28) as usize;
        let extra_len = u16_at(fixed, 30) as usize;
        let comment_len = u16_at(fixed, 32) as usize;
        let name_start = at + CENTRAL_RECORD_LEN as usize;
        let extra_start = name_start + name_len;
        let end = extra_start + extra_len + comment_len;
        if end > directory.len() {
            return Err(bad(at));
        }
        let mut record = Central {
            name: &directory[name_start..extra_start],
            flags: u16_at(fixed, 8),
            method: u16_at(fixed, 10),
            crc32: u32_at(fixed, 16) as u32,
            compressed_size: u32_at(fixed, 20),
            size: u32_at(fixed, 24),
            header_offset: u32_at(fixed, 42),
            symlink: u16_at(fixed, 4) >> 8 == UNIX_HOST
                && (u32_at(fixed, 38) >> 16) & UNIX_TYPE_MASK == UNIX_SYMLINK,
        };
        let mut disk = u16_at(fixed, 34);
        apply_zip64(
            &mut record,
            &mut disk,
            &directory[extra_start..extra_start + extra_len],
        )
        .ok_or_else(|| bad(at))?;
        if disk != 0 {
            return Err(format!("{:?} starts on another disk", record.name));
        }

        records.push(record);
        at = end;
    }
    if at != directory.len() {
        return Err(format!(
            "the central directory holds {} bytes past its {entries} records",
            directory.len() - at
        ));
    }
    Ok(records)
}

/// Replaces each saturated 32-bit field with its value from the Zip64 extra field, which lists
/// them in a fixed order and only when saturated: the three 8-byte fields, then a 4-byte disk
/// number. `None` when the field is missing or short.
fn apply_zip64(record: &mut Central<'_>, disk: &mut u64, mut extra: &[u8]) -> Option<()> {
    let saturated = u64::from(u32::MAX);
    let disk_saturated = *disk == u64::from(u16::MAX);
    let needed = [record.size, record.compressed_size, record.header_offset]
        .iter()
        .filter(|&&value| value == saturated)
        .count();
    if needed == 0 && !disk_saturated {
        return Some(());
    }
    while extra.len() >= 4 {
        let (id, len) = (u16_at(extra, 0), u16_at(extra, 2) as usize);
        let body = extra.get(4..4 + len)?;
        if id == ZIP64_EXTRA {
            let mut rest = body;
            for field in [
                &mut record.size,
                &mut record.compressed_size,
                &mut record.header_offset,
            ] {
                if *field == saturated {
                    *field = u64_at(rest.get(..8)?, 0);
                    rest = &rest[8..];
                }
            }
            if disk_saturated {
                *disk = u32_at(rest.get(..4)?, 0);
            }
            return Some(());
        }
        extra = &extra[4 + len..];
    }
    None
}

/// Where the central directory is and how many records it holds.
struct Footer {
    entries: u64,
    directory_offset: u64,
    directory_size: u64,
}

/// The central directory the end record declares, from its Zip64 version when a Zip64 locator
/// precedes it. The directory must end exactly where the end records begin.
fn footer(file: &File, file_len: u64) -> Result<Footer, IndexError> {
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
    let (declared, directory_end) = match locator {
        None => {
            if u32_at(eocd, 12) == u64::from(u32::MAX) || u32_at(eocd, 16) == u64::from(u32::MAX) {
                return Err("Zip64 sizes without a Zip64 locator".to_owned().into());
            }
            check_disk_counts(u16_at(eocd, 8), u16_at(eocd, 10))?;
            let footer = Footer {
                entries: u16_at(eocd, 10),
                directory_offset: u32_at(eocd, 16),
                directory_size: u32_at(eocd, 12),
            };
            (footer, eocd_offset)
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
            // The extensible data this sizes is never read, but a huge value means a broken record.
            let record_size = u64_at(&record, 4);
            if !(ZIP64_EOCD_LEN - 12..=ZIP64_EOCD_LEN - 12 + MAX_ZIP64_EXTENSIBLE)
                .contains(&record_size)
                || record_offset + 12 + record_size > eocd_offset - 20
            {
                return Err(format!("a Zip64 record of {record_size} bytes").into());
            }
            check_disk_counts(u64_at(&record, 24), u64_at(&record, 32))?;
            let footer = Footer {
                entries: u64_at(&record, 32),
                directory_offset: u64_at(&record, 48),
                directory_size: u64_at(&record, 40),
            };
            (footer, record_offset)
        }
    };
    if declared.directory_size < declared.entries.saturating_mul(CENTRAL_RECORD_LEN)
        || declared
            .directory_offset
            .checked_add(declared.directory_size)
            != Some(directory_end)
    {
        return Err(format!(
            "a central directory of {} bytes at {} cannot hold {} members and end at {directory_end}",
            declared.directory_size, declared.directory_offset, declared.entries
        )
        .into());
    }
    Ok(declared)
}

/// Only single-disk archives are supported, so the members on this disk are all of them.
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
