//! `asset.inspect`: one asset's handles, load states and errors, and a summary of what Bevy made
//! of it. The runtime twin of `ename_gltf_info`: the file says what was authored, this what was
//! loaded.

use std::error::Error;

use bevy::{
    asset::{
        AssetPath, DependencyLoadState, LoadState, RecursiveDependencyLoadState, UntypedAssetId,
    },
    gltf::{Gltf, GltfMaterial},
    image::Image,
    mesh::{Indices, Mesh, VertexAttributeValues},
    prelude::*,
};
use ename_asset_alias::{ALIAS_SOURCE, ContentIndex, validate_alias};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{State, error_text, failures::AssetLoadFailures, type_path};
use crate::command::{Call, CommandError};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct InspectParams {
    /// An alias with an optional label (`examples::airship#Mesh0/Primitive0`), an `alias://`
    /// path, or a path under the asset root.
    asset: String,
}

#[derive(Serialize, JsonSchema)]
pub(super) struct Inspected {
    asset: String,
    handles: Vec<InspectedHandle>,
}

#[derive(Serialize, JsonSchema)]
struct InspectedHandle {
    /// The asset path this handle is registered under.
    path: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    type_path: Option<String>,
    state: State,
    /// Direct dependencies.
    dependencies: State,
    recursive_dependencies: State,
    /// The failure and each error it exposes as its source, outermost first. A loader's own
    /// error is wrapped by Bevy and usually ends the chain, with its message in the first entry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<String>,
    /// Why a dependency failed, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency_error: Option<String>,
    /// The last logged failure for this path, when it has loaded since.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_error: Option<String>,
    /// The handle is gone; state and error come from the failure log.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dropped: bool,
    /// What the loaded value holds, for `Image`, `Mesh`, `StandardMaterial`, `GltfMaterial` and
    /// `Gltf`.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<Value>,
}

pub(super) fn inspect(
    In(call): In<Call<InspectParams>>,
    world: &World,
) -> Result<Inspected, CommandError> {
    let asset = call.params.asset;
    let server = world.resource::<AssetServer>();
    let latest = world.resource::<AssetLoadFailures>().latest_by_path();
    let mut handles = Vec::new();
    for path in candidates(world, &asset)? {
        let logged = latest.get(&path.to_string()).copied();
        let ids = server.get_path_ids(&path);
        match logged {
            // Every handle dropped, but the log still knows why it failed.
            Some(failure) if ids.is_empty() && !failure.resolved => {
                handles.push(InspectedHandle {
                    path: path.to_string(),
                    type_path: None,
                    state: State::Failed,
                    dependencies: State::NotLoaded,
                    recursive_dependencies: State::NotLoaded,
                    errors: vec![failure.error.clone()],
                    dependency_error: None,
                    previous_error: None,
                    dropped: true,
                    summary: None,
                });
            }
            _ => handles.extend(ids.into_iter().map(|id| {
                inspect_handle(
                    world,
                    id,
                    &path,
                    logged.map(|failure| failure.error.as_str()),
                )
            })),
        }
    }
    if handles.is_empty() {
        return Err(CommandError::failed(
            "not_loaded",
            format!(
                "no live handle for `{asset}`: it was never requested, or every handle dropped"
            ),
        )
        .with_hint("`asset.list` lists what is loaded and what failed"));
    }
    Ok(Inspected { asset, handles })
}

/// The asset paths `asset` may be registered under: for an alias, both its `alias://` path and
/// its physical path, label kept. Parsing never panics: `asset` is whatever the caller sent.
fn candidates(world: &World, asset: &str) -> Result<Vec<AssetPath<'static>>, CommandError> {
    let parse = |text: &str| {
        AssetPath::try_parse(text)
            .map(AssetPath::into_owned)
            .map_err(|err| CommandError::invalid("invalid_params", format!("`{asset}`: {err}")))
    };
    let alias_prefix = format!("{ALIAS_SOURCE}://");
    let (base, label) = match asset.split_once('#') {
        Some((base, label)) => (base, Some(label)),
        None => (asset, None),
    };
    let alias = match base.strip_prefix(&alias_prefix) {
        Some(alias) => alias,
        None if !base.contains("://") && base.contains("::") => base,
        None => return Ok(vec![parse(asset)?]),
    };
    validate_alias(alias)
        .map_err(|err| CommandError::invalid("invalid_params", format!("`{asset}`: {err}")))?;
    let mut paths = vec![parse(&format!("{alias_prefix}{alias}"))?];
    if let Some(physical) = world
        .get_resource::<ContentIndex>()
        .and_then(|index| index.resolve(alias))
    {
        paths.push(AssetPath::from_path(physical).into_owned());
    }
    Ok(match label {
        Some(label) => paths
            .into_iter()
            .map(|path| path.with_label(label.to_owned()))
            .collect(),
        None => paths,
    })
}

fn inspect_handle(
    world: &World,
    id: UntypedAssetId,
    path: &AssetPath,
    logged: Option<&str>,
) -> InspectedHandle {
    let server = world.resource::<AssetServer>();
    let (state, dependencies, recursive) = server.get_load_states(id).unwrap_or((
        LoadState::NotLoaded,
        DependencyLoadState::NotLoaded,
        RecursiveDependencyLoadState::NotLoaded,
    ));
    let errors = match &state {
        LoadState::Failed(error) => {
            let mut chain = vec![error_text(error)];
            let mut source = error.source();
            while let Some(error) = source {
                chain.push(error_text(&error));
                source = error.source();
            }
            chain
        }
        _ => Vec::new(),
    };
    // Bevy copies an asset's own failure into its dependency states too.
    let dependency_error = match &recursive {
        RecursiveDependencyLoadState::Failed(cause) if errors.is_empty() => Some(error_text(cause)),
        _ => None,
    };
    InspectedHandle {
        path: path.to_string(),
        type_path: type_path(world, id),
        state: State::of_load(&state),
        dependencies: match dependencies {
            DependencyLoadState::NotLoaded => State::NotLoaded,
            DependencyLoadState::Loading => State::Loading,
            DependencyLoadState::Loaded => State::Loaded,
            DependencyLoadState::Failed(_) => State::Failed,
        },
        recursive_dependencies: State::of_dependencies(&recursive),
        previous_error: logged.filter(|_| errors.is_empty()).map(str::to_owned),
        errors,
        dependency_error,
        dropped: false,
        summary: summary(world, id, path),
    }
}

fn summary(world: &World, id: UntypedAssetId, path: &AssetPath) -> Option<Value> {
    fn get<A: Asset>(world: &World, id: UntypedAssetId) -> Option<&A> {
        world
            .get_resource::<Assets<A>>()?
            .get(id.try_typed::<A>().ok()?)
    }
    if let Some(image) = get::<Image>(world, id) {
        return Some(image_summary(image));
    }
    if let Some(mesh) = get::<Mesh>(world, id) {
        return Some(mesh_summary(mesh));
    }
    if let Some(material) = get::<StandardMaterial>(world, id) {
        return Some(json!({
            "alpha_mode": format!("{:?}", material.alpha_mode),
            "base_color": material.base_color.to_srgba().to_f32_array(),
            "double_sided": material.double_sided,
            "unlit": material.unlit,
            "textures": textures(world, &[
                ("base_color_texture", &material.base_color_texture),
                ("emissive_texture", &material.emissive_texture),
                ("metallic_roughness_texture", &material.metallic_roughness_texture),
                ("normal_map_texture", &material.normal_map_texture),
                ("occlusion_texture", &material.occlusion_texture),
            ]),
        }));
    }
    if let Some(material) = get::<GltfMaterial>(world, id) {
        // bevy_pbr turns `#MaterialN` into the `StandardMaterial` at `#MaterialN/std`.
        let rendered = path.label().map(|label| {
            path.clone()
                .into_owned()
                .with_label(format!("{label}/std"))
                .to_string()
        });
        return Some(json!({
            "alpha_mode": format!("{:?}", material.alpha_mode),
            "base_color": material.base_color.to_srgba().to_f32_array(),
            "double_sided": material.double_sided,
            "unlit": material.unlit,
            "textures": textures(world, &[
                ("base_color_texture", &material.base_color_texture),
                ("emissive_texture", &material.emissive_texture),
                ("metallic_roughness_texture", &material.metallic_roughness_texture),
                ("normal_map_texture", &material.normal_map_texture),
                ("occlusion_texture", &material.occlusion_texture),
            ]),
            "standard_material": rendered,
        }));
    }
    if let Some(gltf) = get::<Gltf>(world, id) {
        return Some(gltf_summary(world, gltf));
    }
    None
}

fn image_summary(image: &Image) -> Value {
    let descriptor = &image.texture_descriptor;
    json!({
        "size": [descriptor.size.width, descriptor.size.height],
        "layers": descriptor.size.depth_or_array_layers,
        "format": format!("{:?}", descriptor.format),
        "mip_levels": descriptor.mip_level_count,
        "sampler": format!("{:?}", image.sampler),
        "asset_usage": format!("{:?}", image.asset_usage),
        "main_world_data": image.data.is_some(),
    })
}

fn mesh_summary(mesh: &Mesh) -> Value {
    let topology = format!("{:?}", mesh.primitive_topology());
    let Ok(attributes) = mesh.try_attributes() else {
        // `RenderAssetUsages` without `MAIN_WORLD`: the data went to the GPU.
        return json!({
            "topology": topology,
            "asset_usage": format!("{:?}", mesh.asset_usage),
            "main_world_data": false,
        });
    };
    let mut vertices = None;
    let mut aabb = None;
    let attributes: Vec<Value> = attributes
        .map(|(attribute, values)| {
            vertices = Some(vertices.map_or(values.len(), |n: usize| n.min(values.len())));
            if attribute.id == Mesh::ATTRIBUTE_POSITION.id
                && let VertexAttributeValues::Float32x3(positions) = values
                && !positions.is_empty()
            {
                let mut min = [f32::INFINITY; 3];
                let mut max = [f32::NEG_INFINITY; 3];
                for position in positions {
                    for axis in 0..3 {
                        min[axis] = min[axis].min(position[axis]);
                        max[axis] = max[axis].max(position[axis]);
                    }
                }
                aabb = Some(json!({"min": min, "max": max}));
            }
            let mut entry = json!({
                "name": attribute.name,
                "format": format!("{:?}", attribute.format),
            });
            if let Some((components, data)) = components(values) {
                let mut min = vec![f32::INFINITY; components];
                let mut max = vec![f32::NEG_INFINITY; components];
                for vertex in data.chunks_exact(components.max(1)) {
                    for (c, value) in vertex.iter().enumerate() {
                        min[c] = min[c].min(*value);
                        max[c] = max[c].max(*value);
                    }
                }
                if !data.is_empty() {
                    entry["min"] = json!(min);
                    entry["max"] = json!(max);
                }
            }
            entry
        })
        .collect();
    let indices = mesh.try_indices().ok().map(|indices| match indices {
        Indices::U16(values) => json!({"count": values.len(), "type": "u16"}),
        Indices::U32(values) => json!({"count": values.len(), "type": "u32"}),
    });
    json!({
        "topology": topology,
        "vertices": vertices.unwrap_or(0),
        "indices": indices,
        "aabb": aabb,
        "attributes": attributes,
        "asset_usage": format!("{:?}", mesh.asset_usage),
        "main_world_data": true,
    })
}

/// Each vertex's components as `f32`, as a shader reads them (normalized formats mapped to 0..1
/// or -1..1), with the component count. `None` for the packed and half-float formats.
fn components(values: &VertexAttributeValues) -> Option<(usize, Vec<f32>)> {
    use VertexAttributeValues as V;
    fn flat<T: Copy, const N: usize>(values: &[[T; N]], f: impl Fn(T) -> f32) -> (usize, Vec<f32>) {
        (N, values.iter().flatten().map(|value| f(*value)).collect())
    }
    fn one<T: Copy>(values: &[T], f: impl Fn(T) -> f32) -> (usize, Vec<f32>) {
        (1, values.iter().map(|value| f(*value)).collect())
    }
    let unorm8 = |v: u8| f32::from(v) / 255.0;
    let snorm8 = |v: i8| (f32::from(v) / 127.0).max(-1.0);
    let unorm16 = |v: u16| f32::from(v) / 65535.0;
    let snorm16 = |v: i16| (f32::from(v) / 32767.0).max(-1.0);
    Some(match values {
        V::Float32(v) => one(v, |x| x),
        V::Float32x2(v) => flat(v, |x| x),
        V::Float32x3(v) => flat(v, |x| x),
        V::Float32x4(v) => flat(v, |x| x),
        V::Float64(v) => one(v, |x| x as f32),
        V::Float64x2(v) => flat(v, |x| x as f32),
        V::Float64x3(v) => flat(v, |x| x as f32),
        V::Float64x4(v) => flat(v, |x| x as f32),
        V::Uint8(v) => one(v, f32::from),
        V::Uint8x2(v) => flat(v, f32::from),
        V::Uint8x4(v) => flat(v, f32::from),
        V::Sint8(v) => one(v, f32::from),
        V::Sint8x2(v) => flat(v, f32::from),
        V::Sint8x4(v) => flat(v, f32::from),
        V::Unorm8(v) => one(v, unorm8),
        V::Unorm8x2(v) => flat(v, unorm8),
        V::Unorm8x4(v) => flat(v, unorm8),
        // Stored B, G, R, A; the shader reads R, G, B, A.
        V::Unorm8x4Bgra(v) => {
            let rgba: Vec<[u8; 4]> = v.iter().map(|[b, g, r, a]| [*r, *g, *b, *a]).collect();
            flat(&rgba, unorm8)
        }
        V::Snorm8(v) => one(v, snorm8),
        V::Snorm8x2(v) => flat(v, snorm8),
        V::Snorm8x4(v) => flat(v, snorm8),
        V::Uint16(v) => one(v, f32::from),
        V::Uint16x2(v) => flat(v, f32::from),
        V::Uint16x4(v) => flat(v, f32::from),
        V::Sint16(v) => one(v, f32::from),
        V::Sint16x2(v) => flat(v, f32::from),
        V::Sint16x4(v) => flat(v, f32::from),
        V::Unorm16(v) => one(v, unorm16),
        V::Unorm16x2(v) => flat(v, unorm16),
        V::Unorm16x4(v) => flat(v, unorm16),
        V::Snorm16(v) => one(v, snorm16),
        V::Snorm16x2(v) => flat(v, snorm16),
        V::Snorm16x4(v) => flat(v, snorm16),
        V::Uint32(v) => one(v, |x| x as f32),
        V::Uint32x2(v) => flat(v, |x| x as f32),
        V::Uint32x3(v) => flat(v, |x| x as f32),
        V::Uint32x4(v) => flat(v, |x| x as f32),
        V::Sint32(v) => one(v, |x| x as f32),
        V::Sint32x2(v) => flat(v, |x| x as f32),
        V::Sint32x3(v) => flat(v, |x| x as f32),
        V::Sint32x4(v) => flat(v, |x| x as f32),
        V::Float16(_) | V::Float16x2(_) | V::Float16x4(_) | V::Unorm10_10_10_2(_) => return None,
    })
}

/// Each set texture slot, by the path of the image in it.
fn textures(world: &World, slots: &[(&str, &Option<Handle<Image>>)]) -> Value {
    let server = world.resource::<AssetServer>();
    let mut out = serde_json::Map::new();
    for (slot, handle) in slots {
        if let Some(handle) = handle {
            let path = server
                .get_path(handle.id())
                .map_or_else(|| format!("{:?}", handle.id()), |path| path.to_string());
            out.insert((*slot).to_owned(), Value::from(path));
        }
    }
    Value::Object(out)
}

fn gltf_summary(world: &World, gltf: &Gltf) -> Value {
    let server = world.resource::<AssetServer>();
    let labels = |ids: Vec<UntypedAssetId>| -> Vec<String> {
        ids.into_iter()
            .map(|id| {
                server
                    .get_path(id)
                    .and_then(|path| path.label().map(str::to_owned))
                    .unwrap_or_else(|| format!("{id:?}"))
            })
            .collect()
    };
    let names = |names: Vec<&str>| {
        let mut names: Vec<String> = names.into_iter().map(str::to_owned).collect();
        names.sort();
        names
    };
    json!({
        "scenes": {
            "labels": labels(gltf.scenes.iter().map(|h| h.id().untyped()).collect()),
            "named": names(gltf.named_scenes.keys().map(AsRef::as_ref).collect()),
        },
        "meshes": {
            "labels": labels(gltf.meshes.iter().map(|h| h.id().untyped()).collect()),
            "named": names(gltf.named_meshes.keys().map(AsRef::as_ref).collect()),
        },
        "materials": {
            "labels": labels(gltf.materials.iter().map(|h| h.id().untyped()).collect()),
            "named": names(gltf.named_materials.keys().map(AsRef::as_ref).collect()),
        },
        "nodes": {
            "labels": labels(gltf.nodes.iter().map(|h| h.id().untyped()).collect()),
            "named": names(gltf.named_nodes.keys().map(AsRef::as_ref).collect()),
        },
    })
}
