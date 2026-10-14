//! Indexing a plain `.tar`: one positioned 512-byte read per header, skipping over the data.
//!
//! Parsed here rather than with the `tar` crate, which reads GNU long-name and PAX bodies of any
//! declared size into memory before anything can check them. Supported: ustar and GNU headers,
//! GNU long names, and PAX `path` and `size`. Global PAX headers, sparse files, links and special
//! entries are refused.
//!
//! Tar has no payload checksum, so a member read is just its bytes. Archives are treated as
//! immutable while the game runs.

use super::{
    Limits,
    files::read_at,
    index::{ArchiveIndex, IndexBuilder, IndexError, Member, Storage},
};
use std::fs::File;

const BLOCK: u64 = 512;
/// The largest GNU long-name or PAX body accepted.
const MAX_EXTENSION_LEN: u64 = 64 * 1024;

pub(crate) fn index(file: &File, limits: &Limits) -> Result<ArchiveIndex, IndexError> {
    let file_len = file.metadata()?.len();
    let mut builder = IndexBuilder::new(limits);
    let mut offset = 0;
    let mut long_path: Option<String> = None;
    let mut pax_size: Option<u64> = None;
    while offset + BLOCK <= file_len {
        let header = read_at(file, offset, BLOCK)?;
        if header.iter().all(|&byte| byte == 0) {
            break;
        }
        verify_checksum(&header, offset)?;
        let typeflag = header[156];
        let declared = parse_number(&header[124..136])
            .ok_or_else(|| format!("bad size field at offset {offset}"))?;
        let entry = matches!(typeflag, b'0' | 0 | b'7' | b'5');
        // An extension header applies to the next real entry, so only that entry consumes it.
        let path = match long_path.take_if(|_| entry) {
            Some(path) => path,
            None => header_path(&header)?,
        };
        let size = match pax_size.take_if(|_| entry) {
            Some(size) if typeflag != b'5' => size,
            _ => declared,
        };
        let data = offset + BLOCK;
        let next = size
            .checked_next_multiple_of(BLOCK)
            .and_then(|padded| data.checked_add(padded))
            .filter(|&next| next <= file_len)
            .ok_or_else(|| format!("member {path:?} runs past the end of the file"))?;
        match typeflag {
            b'L' | b'x' => {
                builder.count_record()?;
                if size > MAX_EXTENSION_LEN {
                    return Err(format!("a {size}-byte extension header at offset {offset}").into());
                }
                let body = read_at(file, data, size)?;
                if typeflag == b'L' {
                    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
                    long_path = Some(utf8(&body[..end])?.to_owned());
                } else {
                    apply_pax(&body, &mut long_path, &mut pax_size)?;
                }
            }
            b'0' | 0 | b'7' => builder.add_file(
                &path,
                Member {
                    size,
                    storage: Storage::Tar { offset: data },
                },
            )?,
            b'5' => builder.add_dir(&path)?,
            b'g' => {
                return Err(
                    format!("global PAX header at offset {offset} is not supported").into(),
                );
            }
            other => {
                return Err(format!(
                    "member {path:?} has entry type {:?}; only files and directories are supported",
                    char::from(other)
                )
                .into());
            }
        }
        offset = next;
    }
    Ok(builder.finish()?)
}

/// The name field, with the ustar prefix when the header is POSIX ustar. GNU headers keep other
/// data where the prefix would be.
fn header_path(header: &[u8]) -> Result<String, String> {
    let field = |range: std::ops::Range<usize>| {
        let bytes = &header[range];
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        utf8(&bytes[..end])
    };
    let name = field(0..100)?;
    if &header[257..263] == b"ustar\0" {
        let prefix = field(345..500)?;
        if !prefix.is_empty() {
            return Ok(format!("{prefix}/{name}"));
        }
    }
    Ok(name.to_owned())
}

/// Octal, or base-256 when the high bit of the first byte is set.
fn parse_number(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        if field[0] & 0x40 != 0 {
            return None;
        }
        return field[1..]
            .iter()
            .try_fold(u64::from(field[0] & 0x3f), |n, &b| {
                n.checked_mul(256)?.checked_add(u64::from(b))
            });
    }
    let text = std::str::from_utf8(field).ok()?;
    let text = text.trim_matches(|c: char| c == '\0' || c == ' ');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

fn verify_checksum(header: &[u8], offset: u64) -> Result<(), String> {
    let stored = parse_number(&header[148..156]);
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    if stored != Some(sum) {
        return Err(format!("bad header checksum at offset {offset}"));
    }
    Ok(())
}

/// Applies the PAX records the next entry needs, refusing sparse files.
fn apply_pax(body: &[u8], path: &mut Option<String>, size: &mut Option<u64>) -> Result<(), String> {
    let mut rest = body;
    while !rest.is_empty() {
        let bad = || "malformed PAX header".to_owned();
        let space = rest.iter().position(|&b| b == b' ').ok_or_else(bad)?;
        let len: usize = utf8(&rest[..space])?.parse().map_err(|_| bad())?;
        if len <= space || len > rest.len() || rest[len - 1] != b'\n' {
            return Err(bad());
        }
        let record = utf8(&rest[space + 1..len - 1])?;
        let (key, value) = record.split_once('=').ok_or_else(bad)?;
        match key {
            "path" => *path = Some(value.to_owned()),
            "size" => *size = Some(value.parse().map_err(|_| bad())?),
            _ if key.starts_with("GNU.sparse") => {
                return Err("sparse files are not supported".to_owned());
            }
            _ => {}
        }
        rest = &rest[len..];
    }
    Ok(())
}

fn utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|_| format!("{bytes:?} is not UTF-8"))
}
