//! `ename_gltf_info` -- what is inside a `.glb` or `.gltf`: scenes and their bounds, nodes,
//! meshes with each attribute's range read from the data, materials, images, and notes on the
//! things that make Bevy render a file differently from what its author saw.
//!
//! Ranges come from the data, not the accessor's optional `min`/`max`. A file the typed API
//! cannot walk safely (a bad index, an invalid enum, a truncated container) gets a summary read
//! straight from its JSON instead of a panic.

mod accessor;
mod raw;

use crate::{
    image_info,
    read_asset::{AssetFile, human_size},
};
use accessor::Buffers;
use gltf::{
    Accessor, Document, Gltf, Material, Node, Primitive, Semantic,
    image::Source as ImageSource,
    json::validation::{Error as ValidationError, Validate},
    material::AlphaMode,
    mesh::Mode,
};
use std::collections::{HashMap, HashSet};

/// What the `bevy_gltf` this workspace is built with accepts in `extensionsRequired`: `gltf`
/// refuses to load anything else, and this crate uses `bevy_gltf`'s exact `gltf` features.
const LOADABLE_EXTENSIONS: &[&str] = gltf::json::extensions::ENABLED_EXTENSIONS;

/// Most nodes listed, and most meshes listed without `--mesh`.
const NODE_LIMIT: usize = 200;
const MESH_LIMIT: usize = 100;

/// `--mesh` and `--images`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GltfOptions {
    /// Only meshes whose name contains this, ignoring case.
    pub mesh: Option<String>,
    /// Decode each image and print its `ename_image_info` summary.
    pub images: bool,
}

/// One file's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GltfReport {
    pub text: String,
    /// `false` when the file was malformed and only its raw JSON was summarized.
    pub read_fully: bool,
}

/// The whole report for one file.
pub fn gltf_info(file: &AssetFile, options: &GltfOptions) -> GltfReport {
    let container = if file.bytes.starts_with(b"glTF") {
        "glb"
    } else {
        "gltf"
    };
    let header = format!(
        "{}  {container} {}",
        file.name(),
        human_size(file.bytes.len())
    );
    let problems = raw::preflight(&file.bytes);
    if !problems.is_empty() {
        return raw_report(header, file, &problems);
    }
    // Last resort: `gltf` unwraps in places no check here foresaw.
    match without_panicking(|| typed_report(&header, file, options)) {
        Ok(Ok(text)) => GltfReport {
            text,
            read_fully: true,
        },
        Ok(Err(errors)) => raw_report(header, file, &errors),
        Err(panic) => raw_report(header, file, &[format!("gltf panicked: {panic}")]),
    }
}

/// The report through `gltf`'s typed API, or the errors that make that unsafe.
fn typed_report(
    header: &str,
    file: &AssetFile,
    options: &GltfOptions,
) -> Result<String, Vec<String>> {
    let gltf =
        Gltf::from_slice_without_validation(&file.bytes).map_err(|err| vec![err.to_string()])?;
    let mut unsupported = Vec::new();
    let mut fatal = Vec::new();
    let root = gltf.as_json();
    root.validate(root, gltf::json::Path::new, &mut |path, error| {
        let line = format!("{}: {error}", path());
        if error == ValidationError::Unsupported {
            unsupported.push(line);
        } else {
            fatal.push(line);
        }
    });
    if !fatal.is_empty() {
        fatal.extend(unsupported);
        return Err(fatal);
    }
    Ok(Report::new(&gltf.document, gltf.blob.as_deref(), file, options).render(header.to_owned()))
}

/// Runs `f`, turning a panic into its message, with the default hook silenced so the panic does
/// not also print.
fn without_panicking<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    result.map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned())
    })
}

fn raw_report(header: String, file: &AssetFile, errors: &[String]) -> GltfReport {
    let mut out = vec![format!(
        "{header}  (not read through gltf; summary from the raw JSON)"
    )];
    out.extend(errors.iter().map(|error| format!("  ! {error}")));
    match raw::json_bytes(&file.bytes)
        .and_then(|json| serde_json::from_slice(json).map_err(|err| err.to_string()))
    {
        Ok(json) => out.extend(raw::summary(&json)),
        Err(err) => out.push(format!("  ! JSON unreadable: {err}")),
    }
    out.push(String::new());
    GltfReport {
        text: out.join("\n"),
        read_fully: false,
    }
}

/// Per-channel range of one attribute's data.
struct Range {
    min: Vec<f32>,
    max: Vec<f32>,
    mean: Vec<f32>,
}

impl Range {
    fn of(values: &[f32], components: usize) -> Option<Self> {
        if values.is_empty() || components == 0 {
            return None;
        }
        let mut min = vec![f32::INFINITY; components];
        let mut max = vec![f32::NEG_INFINITY; components];
        let mut sum = vec![0.0_f64; components];
        for element in values.chunks_exact(components) {
            for (c, value) in element.iter().enumerate() {
                min[c] = min[c].min(*value);
                max[c] = max[c].max(*value);
                sum[c] += f64::from(*value);
            }
        }
        let count = (values.len() / components) as f64;
        let mean = sum.iter().map(|s| (s / count) as f32).collect();
        Some(Self { min, max, mean })
    }
}

struct Report<'a> {
    document: &'a Document,
    buffers: Buffers,
    file: &'a AssetFile,
    options: &'a GltfOptions,
    /// Accessor data read once, by accessor index.
    data: HashMap<usize, Result<Vec<f32>, String>>,
    notes: Vec<String>,
}

impl<'a> Report<'a> {
    fn new(
        document: &'a Document,
        blob: Option<&[u8]>,
        file: &'a AssetFile,
        options: &'a GltfOptions,
    ) -> Self {
        Self {
            document,
            buffers: Buffers::load(document, blob, file),
            file,
            options,
            data: HashMap::new(),
            notes: Vec::new(),
        }
    }

    fn read(&mut self, accessor: &Accessor) -> Result<&[f32], String> {
        let buffers = &self.buffers;
        self.data
            .entry(accessor.index())
            .or_insert_with(|| accessor::read(accessor, buffers))
            .as_deref()
            .map_err(Clone::clone)
    }

    fn render(mut self, header: String) -> String {
        let mut out = vec![self.headline(header)];
        out.push(self.extensions());
        out.extend(self.scenes());
        let nodes = self.nodes();
        section(&mut out, "nodes", nodes);
        let meshes = self.meshes();
        section(&mut out, "meshes", meshes);
        let materials = self.materials();
        section(&mut out, "materials", materials);
        let images = self.images();
        section(&mut out, "images", images);
        self.notes
            .extend(self.buffers.problems().collect::<Vec<_>>());
        if !self.notes.is_empty() {
            out.push("notes".to_owned());
            out.extend(self.notes.iter().map(|note| format!("  ! {note}")));
        }
        out.push(String::new());
        out.join("\n")
    }

    fn headline(&self, header: String) -> String {
        match &self.document.as_json().asset.generator {
            Some(generator) => format!("{header}  generator \"{generator}\""),
            None => header,
        }
    }

    fn extensions(&mut self) -> String {
        let json = self.document.as_json();
        for required in &json.extensions_required {
            if !LOADABLE_EXTENSIONS.contains(&required.as_str()) {
                self.notes.push(format!(
                    "extension {required} is required, and bevy_gltf cannot load it"
                ));
            }
        }
        let list = |names: &[String]| {
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(", ")
            }
        };
        format!(
            "extensions used: {}   required: {}",
            list(&json.extensions_used),
            list(&json.extensions_required)
        )
    }

    fn scenes(&mut self) -> Vec<String> {
        let default = self.document.default_scene().map(|scene| scene.index());
        let scenes: Vec<_> = self.document.scenes().collect();
        scenes
            .into_iter()
            .map(|scene| {
                let mut bounds = Bounds::default();
                let mut visited = Visited::default();
                // An explicit stack: a long chain of nodes must not overflow the call stack.
                let mut stack: Vec<(Node, Mat4)> =
                    scene.nodes().map(|node| (node, IDENTITY)).collect();
                while let Some((node, parent)) = stack.pop() {
                    // `gltf` validates indices, not the hierarchy: a cycle passes, so each node
                    // is visited once.
                    if !visited.insert(node.index()) {
                        continue;
                    }
                    let world = mul(&parent, &node.transform().matrix());
                    self.mesh_bounds(&node, &world, &mut bounds);
                    stack.extend(node.children().map(|child| (child, world)));
                }
                if let Some(node) = visited.repeated {
                    self.notes.push(format!(
                        "scene {}: node {node} is reached more than once (a cycle, or a child of \
                         two parents); Bevy rejects such a hierarchy, and these bounds count each \
                         node once.",
                        scene.index()
                    ));
                }
                let marker = if Some(scene.index()) == default {
                    " (default)"
                } else {
                    ""
                };
                format!(
                    "scene {}{}{marker}  {}",
                    scene.index(),
                    quoted(scene.name()),
                    bounds.describe()
                )
            })
            .collect()
    }

    /// Adds the corners of `node`'s mesh, placed by `world`, to `bounds`.
    fn mesh_bounds(&mut self, node: &Node, world: &Mat4, bounds: &mut Bounds) {
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let Some(positions) = primitive.get(&Semantic::Positions) else {
                    continue;
                };
                let Some(range) = self
                    .read(&positions)
                    .ok()
                    .and_then(|values| Range::of(values, 3))
                else {
                    continue;
                };
                for corner in 0..8 {
                    let pick = |axis: usize| {
                        if corner & (1 << axis) == 0 {
                            range.min[axis]
                        } else {
                            range.max[axis]
                        }
                    };
                    bounds.add(transform_point(world, [pick(0), pick(1), pick(2)]));
                }
            }
        }
    }

    fn nodes(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut listed = 0;
        let total = self.document.nodes().len();
        for scene in self.document.scenes() {
            let mut visited = Visited::default();
            for node in scene.nodes() {
                node_lines(&node, 1, &mut lines, &mut listed, &mut visited);
            }
        }
        if listed < total {
            lines.push(format!(
                "  ... {} more nodes (listed {listed} of {total}, at most {NODE_LIMIT}, from the scenes)",
                total - listed
            ));
        }
        lines
    }

    fn meshes(&mut self) -> Vec<String> {
        let filter = self.options.mesh.as_ref().map(|f| f.to_lowercase());
        let meshes: Vec<_> = self.document.meshes().collect();
        let mut lines = Vec::new();
        let mut listed = 0;
        for mesh in &meshes {
            let matches = filter.as_ref().is_none_or(|filter| {
                mesh.name()
                    .is_some_and(|name| name.to_lowercase().contains(filter))
            });
            let show = matches && (filter.is_some() || listed < MESH_LIMIT);
            if show {
                listed += 1;
                lines.push(format!("  {}{}", mesh.index(), quoted(mesh.name())));
            }
            for primitive in mesh.primitives() {
                let primitive_lines = self.primitive(mesh.index(), &primitive);
                if show {
                    lines.extend(primitive_lines);
                }
            }
        }
        let hidden = meshes.len() - listed;
        if hidden > 0 {
            let why = if filter.is_some() {
                "not matching --mesh".to_owned()
            } else {
                format!("at most {MESH_LIMIT} listed; narrow with --mesh")
            };
            lines.push(format!("  ... {hidden} more meshes ({why})"));
        }
        lines
    }

    /// The primitive's lines; its notes go to `self.notes` whether or not it is listed.
    fn primitive(&mut self, mesh: usize, primitive: &Primitive) -> Vec<String> {
        let at = format!("mesh {mesh} prim {}", primitive.index());
        let material = primitive.material();
        let vertices = primitive
            .get(&Semantic::Positions)
            .map_or(0, |positions| positions.count());
        let indices = match primitive.indices() {
            Some(indices) => format!(
                "  indices {} ({})",
                indices.count(),
                accessor::type_name(&indices)
            ),
            None => String::new(),
        };
        let mut lines = vec![format!(
            "    prim {}  {}  verts {vertices}{indices}  material {}",
            primitive.index(),
            mode_name(primitive.mode()),
            material_name(&material)
        )];
        let mut attributes: Vec<_> = primitive.attributes().collect();
        attributes.sort_by_key(|(semantic, _)| semantic.to_string());
        for (semantic, accessor) in &attributes {
            lines.push(format!(
                "      {:<11} {:<11} {}",
                semantic.to_string(),
                accessor::type_name(accessor),
                self.attribute(semantic, accessor, &material, &at)
            ));
        }
        self.primitive_notes(primitive, &material, &at);
        lines
    }

    fn attribute(
        &mut self,
        semantic: &Semantic,
        accessor: &Accessor,
        material: &Material,
        at: &str,
    ) -> String {
        let components = accessor.dimensions().multiplicity();
        let values = match self.read(accessor) {
            Ok(values) => values.to_vec(),
            Err(err) => {
                self.notes
                    .push(format!("{at}: {} unreadable: {err}", semantic.to_string()));
                return format!("unreadable: {err}");
            }
        };
        let Some(range) = Range::of(&values, components) else {
            return "no data".to_owned();
        };
        match semantic {
            Semantic::Normals => {
                let (min, max) = values
                    .chunks_exact(components)
                    .map(|n| n.iter().map(|v| v * v).sum::<f32>().sqrt())
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), len| {
                        (lo.min(len), hi.max(len))
                    });
                format!("len {min:.2}..{max:.2}")
            }
            Semantic::Colors(set) => {
                let line = color_line(&range);
                if *set == 0 && components == 4 {
                    self.vertex_alpha_note(&values, material, at);
                }
                line
            }
            _ => format!("min {}  max {}", vector(&range.min), vector(&range.max)),
        }
    }

    /// Bevy's `StandardMaterial` multiplies vertex colour into base colour, alpha included, and
    /// tests that product against the cutoff.
    fn vertex_alpha_note(&mut self, colors: &[f32], material: &Material, at: &str) {
        let base_alpha = material.pbr_metallic_roughness().base_color_factor()[3];
        let alphas: Vec<f32> = colors
            .as_chunks::<4>()
            .0
            .iter()
            .map(|color| color[3] * base_alpha)
            .collect();
        let share = |below: f32| {
            alphas.iter().filter(|alpha| **alpha < below).count() as f32 / alphas.len() as f32
        };
        match material.alpha_mode() {
            AlphaMode::Mask => {
                let cutoff = material.alpha_cutoff().unwrap_or(0.5);
                let below = share(cutoff);
                if below > 0.0 {
                    self.notes.push(format!(
                        "{at}: COLOR_0 alpha < cutoff {cutoff:.2} on {} of vertices. Bevy's \
                         StandardMaterial multiplies vertex colour into base colour, so these \
                         fragments are discarded.",
                        percent(below)
                    ));
                }
            }
            AlphaMode::Blend => {
                let below = share(1.0);
                if below > 0.0 {
                    self.notes.push(format!(
                        "{at}: COLOR_0 alpha < 1 on {} of vertices with alpha mode BLEND, so \
                         those parts render see-through.",
                        percent(below)
                    ));
                }
            }
            AlphaMode::Opaque => {}
        }
    }

    fn primitive_notes(&mut self, primitive: &Primitive, material: &Material, at: &str) {
        if primitive.get(&Semantic::Normals).is_none() {
            let what = if primitive.mode() == Mode::Triangles {
                "Bevy duplicates its vertices and computes flat normals"
            } else {
                "Bevy computes none for this mode"
            };
            self.notes.push(format!(
                "{at}: no NORMAL; {what}, and code reading the file's normals finds none."
            ));
        }
        for (slot, tex_coord) in texture_slots(material) {
            if primitive.get(&Semantic::TexCoords(tex_coord)).is_none() {
                self.notes.push(format!(
                    "{at}: material {} samples {slot} with TEXCOORD_{tex_coord}, which the \
                     primitive does not have.",
                    material_name(material)
                ));
            }
        }
    }

    fn materials(&self) -> Vec<String> {
        self.document
            .materials()
            .map(|material| {
                let pbr = material.pbr_metallic_roughness();
                let alpha = match material.alpha_mode() {
                    AlphaMode::Opaque => "OPAQUE".to_owned(),
                    AlphaMode::Blend => "BLEND".to_owned(),
                    AlphaMode::Mask => {
                        format!("MASK cutoff {:.2}", material.alpha_cutoff().unwrap_or(0.5))
                    }
                };
                let mut line = format!(
                    "  {}  alpha {alpha}  double_sided {}  base_color {}",
                    material_name(&material),
                    yes_no(material.double_sided()),
                    vector(&pbr.base_color_factor())
                );
                if material.unlit() {
                    line.push_str("  unlit");
                }
                for (slot, info) in [
                    ("base_color_texture", pbr.base_color_texture()),
                    (
                        "metallic_roughness_texture",
                        pbr.metallic_roughness_texture(),
                    ),
                    ("emissive_texture", material.emissive_texture()),
                ] {
                    if let Some(info) = info {
                        line.push_str(&format!(
                            "  {slot} image {}",
                            info.texture().source().index()
                        ));
                    }
                }
                if let Some(normal) = material.normal_texture() {
                    line.push_str(&format!(
                        "  normal_texture image {}",
                        normal.texture().source().index()
                    ));
                }
                line
            })
            .collect()
    }

    fn images(&mut self) -> Vec<String> {
        let images: Vec<_> = self.document.images().collect();
        let mut lines = Vec::new();
        for image in images {
            let (source, mime, bytes) = match image.source() {
                ImageSource::View { view, mime_type } => (
                    format!("bufferView {}", view.index()),
                    Some(mime_type.to_owned()),
                    self.buffers.view(&view).map(<[u8]>::to_vec),
                ),
                ImageSource::Uri { uri, mime_type } => {
                    let read = accessor::read_uri(uri, self.file);
                    let mime = mime_type
                        .map(str::to_owned)
                        .or_else(|| read.as_ref().ok().and_then(|(_, mime)| mime.clone()))
                        .or_else(|| mime_from_extension(uri).map(str::to_owned));
                    let source = if uri.starts_with("data:") {
                        "data URI".to_owned()
                    } else {
                        format!("uri \"{uri}\"")
                    };
                    (source, mime, read.map(|(bytes, _)| bytes))
                }
            };
            let mime = mime.unwrap_or_else(|| "unknown type".to_owned());
            let size = match &bytes {
                Ok(bytes) => human_size(bytes.len()),
                Err(err) => format!("unreadable: {err}"),
            };
            let feature = bevy_feature(&mime);
            let needs = feature
                .map(|feature| format!("  (needs Bevy feature \"{feature}\")"))
                .unwrap_or_default();
            lines.push(format!(
                "  {}{}  {mime}  {source}  {size}{needs}",
                image.index(),
                quoted(image.name())
            ));
            if let Some(feature) = feature {
                self.notes.push(format!(
                    "image {}: {mime} needs Bevy feature \"{feature}\"",
                    image.index()
                ));
            }
            if let (true, Ok(bytes)) = (self.options.images, &bytes) {
                match image_info::summarize(bytes) {
                    Ok(summary) => {
                        lines.push(format!("    {}", summary.headline()));
                        lines.extend(
                            summary
                                .detail_lines()
                                .into_iter()
                                .map(|line| format!("    {line}")),
                        );
                    }
                    Err(err) => lines.push(format!("    {err}")),
                }
            }
        }
        lines
    }
}

fn section(out: &mut Vec<String>, title: &str, lines: Vec<String>) {
    out.push(title.to_owned());
    if lines.is_empty() {
        out.push("  none".to_owned());
    }
    out.extend(lines);
}

/// Nodes seen in one scene's walk, and the first one reached twice.
#[derive(Default)]
struct Visited {
    seen: HashSet<usize>,
    repeated: Option<usize>,
}

impl Visited {
    /// Whether `node` is new; records the first repeat.
    fn insert(&mut self, node: usize) -> bool {
        let new = self.seen.insert(node);
        if !new && self.repeated.is_none() {
            self.repeated = Some(node);
        }
        new
    }
}

fn node_lines(
    node: &Node,
    depth: usize,
    lines: &mut Vec<String>,
    listed: &mut usize,
    visited: &mut Visited,
) {
    if *listed >= NODE_LIMIT || !visited.insert(node.index()) {
        return;
    }
    *listed += 1;
    let (translation, rotation, scale) = node.transform().decomposed();
    let rotation = if rotation == [0.0, 0.0, 0.0, 1.0] {
        "identity".to_owned()
    } else {
        vector(&rotation)
    };
    let scale = if scale[0] == scale[1] && scale[1] == scale[2] {
        number(scale[0])
    } else {
        vector(&scale)
    };
    let mesh = node
        .mesh()
        .map(|mesh| format!("  mesh {}", mesh.index()))
        .unwrap_or_default();
    let name = node
        .name()
        .map_or_else(|| format!("node {}", node.index()), str::to_owned);
    lines.push(format!(
        "{}{name}  T {}  R {rotation}  S {scale}{mesh}",
        "  ".repeat(depth),
        vector(&translation)
    ));
    for child in node.children() {
        node_lines(&child, depth + 1, lines, listed, visited);
    }
}

/// Every texture slot a material samples, with the UV set it reads.
fn texture_slots(material: &Material) -> Vec<(&'static str, u32)> {
    let pbr = material.pbr_metallic_roughness();
    let mut slots = Vec::new();
    for (slot, info) in [
        ("base_color_texture", pbr.base_color_texture()),
        (
            "metallic_roughness_texture",
            pbr.metallic_roughness_texture(),
        ),
        ("emissive_texture", material.emissive_texture()),
    ] {
        if let Some(info) = info {
            slots.push((slot, info.tex_coord()));
        }
    }
    if let Some(normal) = material.normal_texture() {
        slots.push(("normal_texture", normal.tex_coord()));
    }
    if let Some(occlusion) = material.occlusion_texture() {
        slots.push(("occlusion_texture", occlusion.tex_coord()));
    }
    slots
}

/// The Bevy cargo feature an image type needs beyond the default `png`.
fn bevy_feature(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpeg"),
        "image/webp" => Some("webp"),
        "image/ktx2" => Some("ktx2"),
        "image/vnd-ms.dds" | "image/vnd.ms-dds" => Some("dds"),
        "image/basis" => Some("basis-universal"),
        _ => None,
    }
}

fn mime_from_extension(uri: &str) -> Option<&'static str> {
    let extension = uri.rsplit('.').next()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ktx2" => "image/ktx2",
        "dds" => "image/vnd-ms.dds",
        "basis" => "image/basis",
        _ => return None,
    })
}

fn color_line(range: &Range) -> String {
    let mut parts: Vec<String> = ["r", "g", "b"]
        .iter()
        .zip(range.min.iter().zip(&range.max))
        .map(|(name, (min, max))| format!("{name} {min:.2}..{max:.2}"))
        .collect();
    if range.min.len() == 4 {
        parts.push(format!(
            "a {:.2}..{:.2} mean {:.2}",
            range.min[3], range.max[3], range.mean[3]
        ));
    }
    parts.join("  ")
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Points => "points",
        Mode::Lines => "lines",
        Mode::LineLoop => "line_loop",
        Mode::LineStrip => "line_strip",
        Mode::Triangles => "triangles",
        Mode::TriangleStrip => "triangle_strip",
        Mode::TriangleFan => "triangle_fan",
    }
}

fn material_name(material: &Material) -> String {
    match material.index() {
        Some(index) => format!("{index}{}", quoted(material.name())),
        None => "default".to_owned(),
    }
}

fn quoted(name: Option<&str>) -> String {
    name.map(|name| format!(" \"{name}\"")).unwrap_or_default()
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn percent(share: f32) -> String {
    format!("{:.0} %", share * 100.0)
}

/// `1`, `0.25`: two decimals, trailing zeros dropped.
fn number(value: f32) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

fn vector(values: &[f32]) -> String {
    let parts: Vec<String> = values.iter().map(|v| number(*v)).collect();
    format!("({})", parts.join(", "))
}

/// Column-major, as glTF stores it.
type Mat4 = [[f32; 4]; 4];

const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for (column, out_column) in out.iter_mut().enumerate() {
        for (row, cell) in out_column.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    out
}

fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (row, value) in out.iter_mut().enumerate() {
        *value = m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row];
    }
    out
}

#[derive(Default)]
struct Bounds(Option<([f32; 3], [f32; 3])>);

impl Bounds {
    fn add(&mut self, point: [f32; 3]) {
        let (min, max) = self.0.get_or_insert((point, point));
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }

    fn describe(&self) -> String {
        match self.0 {
            None => "bounds: no meshes".to_owned(),
            Some((min, max)) => format!(
                "bounds min {} max {} m  size {} x {} x {}",
                vector(&min),
                vector(&max),
                number(max[0] - min[0]),
                number(max[1] - min[1]),
                number(max[2] - min[2])
            ),
        }
    }
}
