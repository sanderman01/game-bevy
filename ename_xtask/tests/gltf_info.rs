//! `ename_gltf_info` over GLB files built in code: the notes it exists for, and malformed files
//! that must print diagnostics rather than panic.

use ename_xtask::{
    gltf_info::{GltfOptions, gltf_info},
    read_asset::{ScanPolicy, read_asset},
};
use serde_json::{Value, json};

/// A GLB container around `json` and `bin`, each chunk padded to four bytes.
fn glb(json: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(json).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let length = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend(b"glTF");
    out.extend(2_u32.to_le_bytes());
    out.extend((length as u32).to_le_bytes());
    out.extend((json.len() as u32).to_le_bytes());
    out.extend(b"JSON");
    out.extend(json);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(b"BIN\0");
    out.extend(bin);
    out
}

/// Three positions, three `u16x4` normalized colours with alpha 0, 0.3 and 0.55, and four bytes
/// standing in for a JPEG.
fn bin() -> Vec<u8> {
    let mut bin = Vec::new();
    for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, 0.0]] {
        for value in position {
            bin.extend(value.to_le_bytes());
        }
    }
    for alpha in [0.0_f32, 0.3, 0.55] {
        for value in [1.0, 1.0, 1.0, alpha] {
            bin.extend(((value * 65535.0).round() as u16).to_le_bytes());
        }
    }
    bin.extend([0xFF, 0xD8, 0xFF, 0xE0]);
    bin
}

/// A `MASK` material over vertex alpha below its cutoff, a primitive without `NORMAL` or
/// `TEXCOORD_0` under a textured material, and an embedded JPEG.
fn document() -> Value {
    json!({
        "asset": {"version": "2.0", "generator": "test"},
        "scene": 0,
        "scenes": [{"name": "Scene", "nodes": [0]}],
        "nodes": [{"name": "tree", "mesh": 0, "translation": [10.0, 0.0, 0.0]}],
        "meshes": [{"name": "tree", "primitives": [{
            "attributes": {"POSITION": 0, "COLOR_0": 1},
            "material": 0
        }]}],
        "materials": [{
            "name": "leaves",
            "alphaMode": "MASK",
            "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}
        }],
        "textures": [{"source": 0}],
        "images": [{"name": "albedo", "bufferView": 2, "mimeType": "image/jpeg"}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 2.0, 0.0]},
            {"bufferView": 1, "componentType": 5123, "normalized": true, "count": 3, "type": "VEC4"}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 24},
            {"buffer": 0, "byteOffset": 60, "byteLength": 4}
        ],
        "buffers": [{"byteLength": 64}]
    })
}

fn report(bytes: &[u8], options: &GltfOptions) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.glb");
    std::fs::write(&path, bytes).unwrap();
    let policy = ScanPolicy {
        asset_root: dir.path(),
        search_paths: &[],
        load_order_path: None,
    };
    let file = read_asset(path.to_str().unwrap(), &policy).unwrap();
    gltf_info(&file, options)
}

#[test]
fn a_well_formed_file_lists_ranges_bounds_and_notes() {
    let out = report(&glb(&document(), &bin()), &GltfOptions::default());
    assert!(out.contains("generator \"test\""), "{out}");
    assert!(
        out.contains("bounds min (10, 0, 0) max (11, 2, 0) m  size 1 x 2 x 0"),
        "{out}"
    );
    assert!(
        out.contains("  tree  T (10, 0, 0)  R identity  S 1  mesh 0"),
        "{out}"
    );
    assert!(
        out.contains("COLOR_0     u16x4 norm  r 1.00..1.00  g 1.00..1.00  b 1.00..1.00  a 0.00..0.55 mean 0.28"),
        "{out}"
    );
    assert!(
        out.contains("alpha MASK cutoff 0.50  double_sided no  base_color (1, 1, 1, 1)  base_color_texture image 0"),
        "{out}"
    );
    assert!(
        out.contains("0 \"albedo\"  image/jpeg  bufferView 2  4 B  (needs Bevy feature \"jpeg\")"),
        "{out}"
    );
    for note in [
        "! mesh 0 prim 0: COLOR_0 alpha < cutoff 0.50 on 67 % of vertices.",
        "! mesh 0 prim 0: no NORMAL; Bevy duplicates its vertices and computes flat normals",
        "! mesh 0 prim 0: material 0 \"leaves\" samples base_color_texture with TEXCOORD_0",
        "! image 0: image/jpeg needs Bevy feature \"jpeg\"",
    ] {
        assert!(out.contains(note), "missing {note:?} in\n{out}");
    }
}

#[test]
fn mesh_filter_and_images_flag() {
    let options = GltfOptions {
        mesh: Some("ROCK".into()),
        images: true,
    };
    let out = report(&glb(&document(), &bin()), &options);
    assert!(!out.contains("    prim 0"), "{out}");
    assert!(
        out.contains("... 1 more meshes (not matching --mesh)"),
        "{out}"
    );
    // Four bytes of JPEG header do not decode; the report says so and goes on.
    assert!(out.contains("images\n  0 \"albedo\""), "{out}");
    assert!(
        out.lines()
            .any(|line| line.starts_with("    Format error decoding Jpeg")),
        "{out}"
    );
}

#[test]
fn an_unsupported_required_extension_is_a_note_not_a_failure() {
    let mut document = document();
    document["extensionsUsed"] = json!(["KHR_draco_mesh_compression"]);
    document["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]);
    let out = report(&glb(&document, &bin()), &GltfOptions::default());
    assert!(
        out.contains(
            "! extension KHR_draco_mesh_compression is required, and bevy_gltf cannot load it"
        ),
        "{out}"
    );
    assert!(out.contains("meshes\n  0 \"tree\""), "{out}");
}

#[test]
fn an_index_out_of_range_falls_back_to_the_raw_summary() {
    let mut document = document();
    document["meshes"][0]["primitives"][0]["material"] = json!(5);
    let out = report(&glb(&document, &bin()), &GltfOptions::default());
    assert!(
        out.contains("(not read through gltf; summary from the raw JSON)"),
        "{out}"
    );
    assert!(out.contains("Index out of bounds"), "{out}");
    assert!(out.contains("meshes  0 \"tree\""), "{out}");
    assert!(
        out.contains("image 0 \"albedo\"  image/jpeg  bufferView 2"),
        "{out}"
    );
}

#[test]
fn an_invalid_enum_falls_back_to_the_raw_summary() {
    let mut document = document();
    document["materials"][0]["alphaMode"] = json!("SOMETIMES");
    let out = report(&glb(&document, &bin()), &GltfOptions::default());
    assert!(out.contains("Invalid value"), "{out}");
    assert!(out.contains("materials  0 \"leaves\""), "{out}");
}

#[test]
fn a_truncated_bin_chunk_falls_back_to_the_raw_summary() {
    let mut bytes = glb(&document(), &bin());
    bytes.truncate(bytes.len() - 10);
    let out = report(&bytes, &GltfOptions::default());
    assert!(
        out.contains("(not read through gltf; summary from the raw JSON)"),
        "{out}"
    );
    assert!(out.contains("counts  scenes 1  nodes 1  meshes 1"), "{out}");
}

#[test]
fn a_buffer_shorter_than_declared_reads_as_unreadable() {
    let mut document = document();
    document["buffers"][0]["byteLength"] = json!(128);
    document["bufferViews"][1]["byteLength"] = json!(90);
    let out = report(&glb(&document, &bin()), &GltfOptions::default());
    assert!(
        out.contains("! buffer 0: buffer 0 holds 64 bytes, needs 128"),
        "{out}"
    );
    assert!(out.contains("unreadable"), "{out}");
}

#[test]
fn an_accessor_past_its_view_reads_as_unreadable() {
    let mut document = document();
    document["accessors"][1]["count"] = json!(30);
    let out = report(&glb(&document, &bin()), &GltfOptions::default());
    assert!(
        out.contains("COLOR_0     u16x4 norm  unreadable: 30 elements"),
        "{out}"
    );
}
