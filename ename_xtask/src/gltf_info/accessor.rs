//! Reading accessor data from the buffers, with every length checked first: `gltf`'s own readers
//! slice without checking, so a short buffer would panic there.

use crate::read_asset::AssetFile;
use base64::Engine;
use gltf::{
    Accessor, Document,
    accessor::{DataType, Dimensions, sparse::IndexType},
    buffer::{Source, View},
};

/// Each buffer's bytes, or why they could not be had.
pub(super) struct Buffers(Vec<Result<Vec<u8>, String>>);

impl Buffers {
    pub(super) fn load(document: &Document, blob: Option<&[u8]>, file: &AssetFile) -> Self {
        Self(
            document
                .buffers()
                .map(|buffer| {
                    let bytes = match buffer.source() {
                        Source::Bin => blob
                            .map(<[u8]>::to_vec)
                            .ok_or_else(|| "no BIN chunk".to_owned()),
                        Source::Uri(uri) => read_uri(uri, file).map(|(bytes, _)| bytes),
                    }?;
                    if bytes.len() < buffer.length() {
                        return Err(format!(
                            "buffer {} holds {} bytes, needs {}",
                            buffer.index(),
                            bytes.len(),
                            buffer.length()
                        ));
                    }
                    Ok(bytes)
                })
                .collect(),
        )
    }

    /// Problems with the buffers themselves, one line each.
    pub(super) fn problems(&self) -> impl Iterator<Item = String> + '_ {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(index, bytes)| Some(format!("buffer {index}: {}", bytes.as_ref().err()?)))
    }

    /// A buffer view's bytes, checked against its buffer.
    pub(super) fn view(&self, view: &View) -> Result<&[u8], String> {
        let buffer = self
            .0
            .get(view.buffer().index())
            .ok_or("buffer missing")?
            .as_ref()
            .map_err(|_| format!("buffer {} unreadable", view.buffer().index()))?;
        let end = view
            .offset()
            .checked_add(view.length())
            .filter(|end| *end <= buffer.len())
            .ok_or_else(|| format!("bufferView {} runs past its buffer", view.index()))?;
        Ok(&buffer[view.offset()..end])
    }
}

/// Reads a data URI or a file next to the asset. Returns the bytes and the data URI's MIME type.
pub(super) fn read_uri(uri: &str, file: &AssetFile) -> Result<(Vec<u8>, Option<String>), String> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (header, payload) = data.split_once(',').ok_or("data URI without a comma")?;
        let mime = header.split(';').next().filter(|m| !m.is_empty());
        if !header.ends_with(";base64") {
            return Err("data URI is not base64".to_owned());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|err| format!("data URI: {err}"))?;
        return Ok((bytes, mime.map(str::to_owned)));
    }
    let path = percent_decode(uri);
    file.read_sibling(&path)
        .map(|bytes| (bytes, None))
        .map_err(|err| err.to_string())
}

/// `%20` and the like, as a glTF URI may hold them.
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| (bytes.get(at)).and_then(|b| (*b as char).to_digit(16));
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every element's components, flat (`count * components` values), as `f32`. Normalized integer
/// types are mapped to 0..1 or -1..1, as a shader would see them.
pub(super) fn read(accessor: &Accessor, buffers: &Buffers) -> Result<Vec<f32>, String> {
    let components = accessor.dimensions().multiplicity();
    let component_size = accessor.data_type().size();
    if is_matrix(accessor.dimensions()) && component_size < 4 {
        return Err("padded small-component matrices are not read".to_owned());
    }
    let element_size = components * component_size;
    let count = accessor.count();
    let normalized = accessor.normalized();
    let mut values = vec![0.0; count * components];
    if let Some(view) = accessor.view() {
        let bytes = buffers.view(&view)?;
        let stride = view.stride().unwrap_or(element_size);
        check_fits(bytes.len(), accessor.offset(), stride, element_size, count)?;
        for element in 0..count {
            let at = accessor.offset() + element * stride;
            read_element(
                &bytes[at..at + element_size],
                accessor.data_type(),
                normalized,
                &mut values[element * components..(element + 1) * components],
            );
        }
    }
    if let Some(sparse) = accessor.sparse() {
        let indices = sparse.indices();
        let index_bytes = buffers.view(&indices.view())?;
        let index_size = match indices.index_type() {
            IndexType::U8 => 1,
            IndexType::U16 => 2,
            IndexType::U32 => 4,
        };
        let values_view = sparse.values();
        let value_bytes = buffers.view(&values_view.view())?;
        let n = sparse.count();
        check_fits(
            index_bytes.len(),
            indices.offset(),
            index_size,
            index_size,
            n,
        )?;
        check_fits(
            value_bytes.len(),
            values_view.offset(),
            element_size,
            element_size,
            n,
        )?;
        for k in 0..n {
            let at = indices.offset() + k * index_size;
            let index = match index_size {
                1 => usize::from(index_bytes[at]),
                2 => usize::from(u16::from_le_bytes([index_bytes[at], index_bytes[at + 1]])),
                _ => u32::from_le_bytes(index_bytes[at..at + 4].try_into().unwrap()) as usize,
            };
            if index >= count {
                return Err(format!("sparse index {index} past count {count}"));
            }
            let at = values_view.offset() + k * element_size;
            read_element(
                &value_bytes[at..at + element_size],
                accessor.data_type(),
                normalized,
                &mut values[index * components..(index + 1) * components],
            );
        }
    }
    Ok(values)
}

fn is_matrix(dimensions: Dimensions) -> bool {
    matches!(
        dimensions,
        Dimensions::Mat2 | Dimensions::Mat3 | Dimensions::Mat4
    )
}

fn check_fits(
    len: usize,
    offset: usize,
    stride: usize,
    element_size: usize,
    count: usize,
) -> Result<(), String> {
    if count == 0 {
        return Ok(());
    }
    let end = stride
        .checked_mul(count - 1)
        .and_then(|n| n.checked_add(offset))
        .and_then(|n| n.checked_add(element_size));
    match end {
        Some(end) if end <= len => Ok(()),
        _ => Err(format!(
            "{count} elements of {element_size} bytes (stride {stride}) from offset {offset} run past a view of {len} bytes"
        )),
    }
}

fn read_element(bytes: &[u8], data_type: DataType, normalized: bool, out: &mut [f32]) {
    let size = data_type.size();
    for (value, chunk) in out.iter_mut().zip(bytes.chunks_exact(size)) {
        *value = match data_type {
            DataType::I8 => {
                let v = f32::from(chunk[0] as i8);
                if normalized { (v / 127.0).max(-1.0) } else { v }
            }
            DataType::U8 => {
                let v = f32::from(chunk[0]);
                if normalized { v / 255.0 } else { v }
            }
            DataType::I16 => {
                let v = f32::from(i16::from_le_bytes([chunk[0], chunk[1]]));
                if normalized {
                    (v / 32767.0).max(-1.0)
                } else {
                    v
                }
            }
            DataType::U16 => {
                let v = f32::from(u16::from_le_bytes([chunk[0], chunk[1]]));
                if normalized { v / 65535.0 } else { v }
            }
            DataType::U32 => u32::from_le_bytes(chunk.try_into().unwrap()) as f32,
            DataType::F32 => f32::from_le_bytes(chunk.try_into().unwrap()),
        };
    }
}

/// `f32x3`, `u16x4 norm`, `u32` for a scalar, `f32 mat4`.
pub(super) fn type_name(accessor: &Accessor) -> String {
    let component = match accessor.data_type() {
        DataType::I8 => "i8",
        DataType::U8 => "u8",
        DataType::I16 => "i16",
        DataType::U16 => "u16",
        DataType::U32 => "u32",
        DataType::F32 => "f32",
    };
    let shape = match accessor.dimensions() {
        Dimensions::Scalar => String::new(),
        Dimensions::Vec2 => "x2".to_owned(),
        Dimensions::Vec3 => "x3".to_owned(),
        Dimensions::Vec4 => "x4".to_owned(),
        Dimensions::Mat2 => " mat2".to_owned(),
        Dimensions::Mat3 => " mat3".to_owned(),
        Dimensions::Mat4 => " mat4".to_owned(),
    };
    let norm = if accessor.normalized() { " norm" } else { "" };
    format!("{component}{shape}{norm}")
}
