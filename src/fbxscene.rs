//! Turning assembled geometry into an FBX file.
//!
//! Coordinates go out untouched. SCS authors its models Y-up, which is also
//! FBX's convention - the Blender addon's own conversion is a single
//! `Rotation(pi/2, X)`, exactly the axis change an FBX importer applies for a
//! Y-up file - so there is nothing to rotate here. Lengths are written in
//! centimetres, which is what `UnitScaleFactor: 1` means to an importer.

use std::io::{BufWriter, Result};
use std::path::Path;

use crate::fbx::{self, p70_c3, p70_d, p70_i, p70_s, Ids, Node};

const CM: f64 = 100.0;
/// name of the second UV layer, the one paint jobs are laid out on
pub const PAINT_UV: &str = "PaintUV";

pub struct Mesh {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// the paint-job UV channel, when the model carries one
    pub uvs2: Vec<[f32; 2]>,
    pub colors: Vec<[f32; 4]>,
    pub tris: Vec<[u32; 3]>,
    pub material: usize,
}

pub struct Material {
    pub name: String,
    pub diffuse: [f32; 3],
    /// path relative to the .fbx, e.g. `textures/body.dds`
    pub texture: Option<String>,
    pub normal_map: Option<String>,
    /// whether this shader means the texture's alpha channel as opacity
    pub wants_alpha: bool,
    /// which UV layer the texture is sampled on
    pub uv_set: String,
}

impl Mesh {
    fn geometry(&self, id: i64) -> Node {
        let mut verts = Vec::with_capacity(self.positions.len() * 3);
        for p in &self.positions {
            verts.push(p[0] as f64 * CM);
            verts.push(p[1] as f64 * CM);
            verts.push(p[2] as f64 * CM);
        }

        // A polygon's last corner is stored with its index bit-flipped; that is
        // how the format marks where one polygon ends and the next begins.
        let mut idx = Vec::with_capacity(self.tris.len() * 3);
        for t in &self.tris {
            idx.push(t[0] as i32);
            idx.push(t[1] as i32);
            idx.push(!(t[2] as i32));
        }

        let corners = self.tris.len() * 3;
        let mut g = Node::new("Geometry")
            .i64(id)
            .objname("Geometry", &self.name)
            .str("Mesh")
            .child(Node::new("GeometryVersion").i32(124))
            .child(Node::new("Vertices").arr_f64(verts))
            .child(Node::new("PolygonVertexIndex").arr_i32(idx));

        // No normal layer is written, on purpose.
        //
        // FBX stores normals per polygon corner, and every importer turns those
        // into custom split normals - a layer that overrides whatever the
        // receiving application would compute, so later smoothing, welding or
        // re-tessellation stops having any effect. Leaving them out is only
        // safe because PIX geometry already splits its vertices at every hard
        // edge, UV seam and vertex-colour break (the reason SCS Blender Tools
        // offers a welding option on import), so normals recomputed from the
        // geometry come out the same as the authored ones.

        if !self.uvs.is_empty() {
            let mut uv = Vec::with_capacity(self.uvs.len() * 2);
            for t in &self.uvs {
                uv.push(t[0] as f64);
                // FBX puts the UV origin at the bottom left, SCS at the top left
                uv.push(1.0 - t[1] as f64);
            }
            let mut uvi = Vec::with_capacity(corners);
            for t in &self.tris {
                for &v in t.iter() {
                    uvi.push(v as i32);
                }
            }
            g = g.child(
                Node::new("LayerElementUV")
                    .i32(0)
                    .child(Node::new("Version").i32(101))
                    .child(Node::new("Name").str("UVMap"))
                    .child(Node::new("MappingInformationType").str("ByPolygonVertex"))
                    .child(Node::new("ReferenceInformationType").str("IndexToDirect"))
                    .child(Node::new("UV").arr_f64(uv))
                    .child(Node::new("UVIndex").arr_i32(uvi)),
            );
        }

        // A paint job is laid out on its own UV channel, not the panel's. The
        // Hyundai HD 1997 makes the point: sampled on UV0 its livery mask paints
        // the whole truck flat magenta, because that is the part of the sheet
        // the panel UVs happen to land on.
        if !self.uvs2.is_empty() {
            let mut uv = Vec::with_capacity(self.uvs2.len() * 2);
            for t in &self.uvs2 {
                uv.push(t[0] as f64);
                uv.push(1.0 - t[1] as f64);
            }
            let mut uvi = Vec::with_capacity(corners);
            for t in &self.tris {
                for &v in t.iter() {
                    uvi.push(v as i32);
                }
            }
            g = g.child(
                Node::new("LayerElementUV")
                    .i32(1)
                    .child(Node::new("Version").i32(101))
                    .child(Node::new("Name").str(PAINT_UV))
                    .child(Node::new("MappingInformationType").str("ByPolygonVertex"))
                    .child(Node::new("ReferenceInformationType").str("IndexToDirect"))
                    .child(Node::new("UV").arr_f64(uv))
                    .child(Node::new("UVIndex").arr_i32(uvi)),
            );
        }

        if !self.colors.is_empty() {
            let mut col = Vec::with_capacity(self.colors.len() * 4);
            for c in &self.colors {
                // SCS stores vertex colour halved and its shader doubles it back
                col.push((c[0] * 2.0).min(1.0) as f64);
                col.push((c[1] * 2.0).min(1.0) as f64);
                col.push((c[2] * 2.0).min(1.0) as f64);
                col.push(c[3] as f64);
            }
            let mut ci = Vec::with_capacity(corners);
            for t in &self.tris {
                for &v in t.iter() {
                    ci.push(v as i32);
                }
            }
            g = g.child(
                Node::new("LayerElementColor")
                    .i32(0)
                    .child(Node::new("Version").i32(101))
                    .child(Node::new("Name").str("Col"))
                    .child(Node::new("MappingInformationType").str("ByPolygonVertex"))
                    .child(Node::new("ReferenceInformationType").str("IndexToDirect"))
                    .child(Node::new("Colors").arr_f64(col))
                    .child(Node::new("ColorIndex").arr_i32(ci)),
            );
        }

        g = g.child(
            Node::new("LayerElementMaterial")
                .i32(0)
                .child(Node::new("Version").i32(101))
                .child(Node::new("Name").str(""))
                .child(Node::new("MappingInformationType").str("AllSame"))
                .child(Node::new("ReferenceInformationType").str("IndexToDirect"))
                .child(Node::new("Materials").arr_i32(vec![0])),
        );

        if !self.uvs2.is_empty() {
            g = g.child(
                Node::new("Layer").i32(1).child(Node::new("Version").i32(100)).child(
                    Node::new("LayerElement")
                        .child(Node::new("Type").str("LayerElementUV"))
                        .child(Node::new("TypedIndex").i32(1)),
                ),
            );
        }

        let mut layer = Node::new("Layer").i32(0).child(Node::new("Version").i32(100));
        for ty in ["LayerElementUV", "LayerElementColor", "LayerElementMaterial"] {
            let present = match ty {
                "LayerElementUV" => !self.uvs.is_empty(),
                "LayerElementColor" => !self.colors.is_empty(),
                _ => true,
            };
            if present {
                layer = layer.child(
                    Node::new("LayerElement")
                        .child(Node::new("Type").str(ty))
                        .child(Node::new("TypedIndex").i32(0)),
                );
            }
        }
        g.child(layer)
    }
}

fn model_node(id: i64, name: &str) -> Node {
    Node::new("Model")
        .i64(id)
        .objname("Model", name)
        .str("Mesh")
        .child(Node::new("Version").i32(232))
        .child(Node::new("Properties70").children([
            p70_i("InheritType", "enum", "", 1),
            p70_c3("ScalingMax", "Vector3D", "Vector", [0.0, 0.0, 0.0]),
            p70_i("DefaultAttributeIndex", "int", "Integer", 0),
        ]))
        .child(Node::new("Shading").bool(true))
        .child(Node::new("Culling").str("CullingOff"))
}

fn material_node(id: i64, m: &Material) -> Node {
    let d = [m.diffuse[0] as f64, m.diffuse[1] as f64, m.diffuse[2] as f64];
    Node::new("Material")
        .i64(id)
        .objname("Material", &m.name)
        .str("")
        .child(Node::new("Version").i32(102))
        .child(Node::new("ShadingModel").str("phong"))
        .child(Node::new("MultiLayer").i32(0))
        .child(Node::new("Properties70").children([
            p70_c3("DiffuseColor", "Color", "", d),
            p70_d("DiffuseFactor", "Number", "", 1.0),
            p70_c3("SpecularColor", "Color", "", [0.1, 0.1, 0.1]),
            p70_d("Shininess", "Number", "", 20.0),
            p70_d("Opacity", "Number", "", 1.0),
        ]))
}

fn texture_nodes(tex_id: i64, vid_id: i64, name: &str, rel: &str, uv_set: &str) -> (Node, Node) {
    let video = Node::new("Video")
        .i64(vid_id)
        .objname("Video", name)
        .str("Clip")
        .child(Node::new("Type").str("Clip"))
        .child(Node::new("Properties70").child(p70_s("Path", "KString", "XRefUrl", rel)))
        .child(Node::new("UseMipMap").i32(0))
        .child(Node::new("Filename").str(rel))
        .child(Node::new("RelativeFilename").str(rel));
    let tex = Node::new("Texture")
        .i64(tex_id)
        .objname("Texture", name)
        .str("")
        .child(Node::new("Type").str("TextureVideoClip"))
        .child(Node::new("Version").i32(202))
        .child(Node::new("TextureName").objname("Texture", name))
        .child(Node::new("Properties70").children([
            p70_s("UVSet", "KString", "", uv_set),
            p70_i("UseMaterial", "bool", "", 1),
        ]))
        .child(Node::new("Media").objname("Video", name))
        .child(Node::new("FileName").str(rel))
        .child(Node::new("RelativeFilename").str(rel))
        .child(Node::new("ModelUVTranslation").f64(0.0).f64(0.0))
        .child(Node::new("ModelUVScaling").f64(1.0).f64(1.0))
        .child(Node::new("Texture_Alpha_Source").str("None"))
        .child(Node::new("Cropping").i32(0).i32(0).i32(0).i32(0));
    (tex, video)
}

pub fn write_file(path: &Path, meshes: &[Mesh], materials: &[Material]) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let mut ids = Ids::default();

    let header = Node::new("FBXHeaderExtension")
        .child(Node::new("FBXHeaderVersion").i32(1003))
        .child(Node::new("FBXVersion").i32(7400))
        .child(Node::new("Creator").str("scs2fbx"));

    let global = Node::new("GlobalSettings")
        .child(Node::new("Version").i32(1000))
        .child(Node::new("Properties70").children([
            p70_i("UpAxis", "int", "Integer", 1),
            p70_i("UpAxisSign", "int", "Integer", 1),
            p70_i("FrontAxis", "int", "Integer", 2),
            p70_i("FrontAxisSign", "int", "Integer", 1),
            p70_i("CoordAxis", "int", "Integer", 0),
            p70_i("CoordAxisSign", "int", "Integer", 1),
            p70_d("UnitScaleFactor", "double", "Number", 1.0),
        ]));

    let doc_id = ids.next();
    let documents = Node::new("Documents")
        .child(Node::new("Count").i32(1))
        .child(
            Node::new("Document")
                .i64(doc_id)
                .str("")
                .str("Scene")
                .child(Node::new("Properties70"))
                .child(Node::new("RootNode").i64(0)),
        );

    let textured: Vec<&Material> = materials.iter().filter(|m| m.texture.is_some()).collect();
    let definitions = Node::new("Definitions")
        .child(Node::new("Version").i32(100))
        .child(Node::new("Count").i32(
            (meshes.len() * 2 + materials.len() + textured.len() * 2 + 1) as i32,
        ))
        .child(Node::new("ObjectType").str("GlobalSettings").child(Node::new("Count").i32(1)))
        .child(Node::new("ObjectType").str("Model").child(Node::new("Count").i32(meshes.len() as i32)))
        .child(Node::new("ObjectType").str("Geometry").child(Node::new("Count").i32(meshes.len() as i32)))
        .child(Node::new("ObjectType").str("Material").child(Node::new("Count").i32(materials.len() as i32)))
        .child(Node::new("ObjectType").str("Texture").child(Node::new("Count").i32(textured.len() as i32)))
        .child(Node::new("ObjectType").str("Video").child(Node::new("Count").i32(textured.len() as i32)));

    let mut objects = Node::new("Objects");
    let mut conns: Vec<Node> = Vec::new();

    let mat_ids: Vec<i64> = materials.iter().map(|_| ids.next()).collect();
    for (i, m) in materials.iter().enumerate() {
        objects = objects.child(material_node(mat_ids[i], m));
        if let Some(rel) = &m.texture {
            let (t, v) = (ids.next(), ids.next());
            let (tn, vn) = texture_nodes(t, v, &m.name, rel, &m.uv_set);
            objects = objects.child(tn).child(vn);
            conns.push(Node::new("C").str("OP").i64(t).i64(mat_ids[i]).str("DiffuseColor"));
            conns.push(Node::new("C").str("OO").i64(v).i64(t));
        }
        if let Some(rel) = &m.normal_map {
            let (t, v) = (ids.next(), ids.next());
            let (tn, vn) = texture_nodes(t, v, &format!("{}_n", m.name), rel, "UVMap");
            objects = objects.child(tn).child(vn);
            conns.push(Node::new("C").str("OP").i64(t).i64(mat_ids[i]).str("NormalMap"));
            conns.push(Node::new("C").str("OO").i64(v).i64(t));
        }
    }

    for mesh in meshes {
        let gid = ids.next();
        let mid = ids.next();
        objects = objects.child(mesh.geometry(gid)).child(model_node(mid, &mesh.name));
        conns.push(Node::new("C").str("OO").i64(gid).i64(mid));
        conns.push(Node::new("C").str("OO").i64(mid).i64(0));
        if let Some(&m) = mat_ids.get(mesh.material) {
            conns.push(Node::new("C").str("OO").i64(m).i64(mid));
        }
    }

    let connections = Node::new("Connections").children(conns);
    fbx::write(
        &mut w,
        &[header, global, documents, Node::new("References"), definitions, objects, connections],
    )?;
    Ok(())
}



