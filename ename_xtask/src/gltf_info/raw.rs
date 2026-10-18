//! The fallback for a file `gltf`'s typed API cannot walk safely: a summary read straight from the
//! JSON, which needs no index to be valid.

use serde_json::Value;

/// The JSON text of a `.gltf`, or the JSON chunk of a `.glb` read without trusting the rest of
/// the container.
pub(super) fn json_bytes(bytes: &[u8]) -> Result<&[u8], String> {
    if !bytes.starts_with(b"glTF") {
        return Ok(bytes);
    }
    let word = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    };
    let length = word(12).ok_or("GLB header is truncated")?;
    if bytes.get(16..20) != Some(b"JSON") {
        return Err("GLB's first chunk is not JSON".to_owned());
    }
    bytes
        .get(20..20usize.saturating_add(length))
        .ok_or_else(|| "GLB's JSON chunk is truncated".to_owned())
}

/// What `gltf` 1.4.1 panics on instead of reporting, checked on the raw bytes and JSON first: a GLB
/// length below its own header (`Glb::from_slice` subtracts unchecked), a `POSITION` accessor out
/// of range (validation indexes it directly), and an image without a `uri` or a `bufferView`
/// without a `mimeType` (`Image::source` unwraps both).
pub(super) fn preflight(bytes: &[u8]) -> Vec<String> {
    let mut problems = Vec::new();
    if bytes.starts_with(b"glTF") {
        let declared = bytes
            .get(8..12)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        match declared {
            Some(length) if (12..=bytes.len()).contains(&length) => {}
            Some(length) => problems.push(format!(
                "GLB header declares {length} bytes; the file has {}",
                bytes.len()
            )),
            None => problems.push("GLB header is truncated".to_owned()),
        }
    }
    let Ok(json) = json_bytes(bytes)
        .and_then(|json| serde_json::from_slice::<Value>(json).map_err(|err| err.to_string()))
    else {
        // `gltf` reports unparseable JSON as an error of its own.
        return problems;
    };
    let accessors = json
        .get("accessors")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let meshes = json.get("meshes").and_then(Value::as_array);
    for (m, mesh) in meshes.into_iter().flatten().enumerate() {
        let primitives = mesh.get("primitives").and_then(Value::as_array);
        for (p, primitive) in primitives.into_iter().flatten().enumerate() {
            let position = primitive.pointer("/attributes/POSITION");
            if let Some(index) = position.and_then(Value::as_u64)
                && index as usize >= accessors
            {
                problems.push(format!(
                    "meshes[{m}].primitives[{p}]: POSITION accessor {index} does not exist \
                     ({accessors} accessors)"
                ));
            }
        }
    }
    let images = json.get("images").and_then(Value::as_array);
    for (i, image) in images.into_iter().flatten().enumerate() {
        match (
            image.get("bufferView"),
            image.get("uri"),
            image.get("mimeType"),
        ) {
            (Some(_), _, None) => {
                problems.push(format!("images[{i}]: bufferView without mimeType"));
            }
            (None, None, _) => problems.push(format!("images[{i}]: neither uri nor bufferView")),
            _ => {}
        }
    }
    problems
}

/// Counts, names, MIME types and extensions, one fact per line.
pub(super) fn summary(json: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let list = |key: &str| json.get(key).and_then(Value::as_array);
    let strings = |key: &str| {
        list(key)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|joined| !joined.is_empty())
            .unwrap_or_else(|| "none".to_owned())
    };
    if let Some(generator) = json.pointer("/asset/generator").and_then(Value::as_str) {
        lines.push(format!("generator \"{generator}\""));
    }
    lines.push(format!(
        "extensions used: {}   required: {}",
        strings("extensionsUsed"),
        strings("extensionsRequired")
    ));
    let counts = [
        "scenes",
        "nodes",
        "meshes",
        "materials",
        "accessors",
        "bufferViews",
        "buffers",
        "images",
        "textures",
    ]
    .into_iter()
    .map(|key| format!("{key} {}", list(key).map_or(0, Vec::len)))
    .collect::<Vec<_>>()
    .join("  ");
    lines.push(format!("counts  {counts}"));
    for key in ["meshes", "materials"] {
        let names = list(key)
            .into_iter()
            .flatten()
            .enumerate()
            .map(
                |(index, item)| match item.get("name").and_then(Value::as_str) {
                    Some(name) => format!("{index} \"{name}\""),
                    None => index.to_string(),
                },
            )
            .collect::<Vec<_>>();
        if !names.is_empty() {
            lines.push(format!("{key}  {}", names.join(", ")));
        }
    }
    for (index, image) in list("images").into_iter().flatten().enumerate() {
        let name = image.get("name").and_then(Value::as_str).unwrap_or("");
        let mime = image
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("no mimeType");
        let source = match (image.get("bufferView"), image.get("uri")) {
            (Some(view), _) => format!("bufferView {view}"),
            (None, Some(Value::String(uri))) if uri.starts_with("data:") => "data URI".to_owned(),
            (None, Some(uri)) => format!("uri {uri}"),
            (None, None) => "no source".to_owned(),
        };
        lines.push(format!("image {index} \"{name}\"  {mime}  {source}"));
    }
    lines
}
