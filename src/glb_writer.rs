//! Minimal GLB writer for the gun splitter.
//!
//! Repacks a set of named mesh nodes (positions + normals + optional UVs,
//! indexed triangles, flat PBR materials) into a single self-contained `.glb`.
//! All node transforms must already be baked into the vertex data — output
//! nodes are flat with identity transforms.
//!
//! Loudly refuses textured materials, skins, morph targets and non-triangle
//! primitives: the splitter only handles static hard-surface gun parts.

use std::collections::BTreeMap;

use gltf_json::validation::Checked;
use gltf_json::{accessor, buffer, material, mesh, scene, Index, Root};

/// One triangle-list primitive with baked world-space vertices.
#[derive(Clone, Debug)]
pub struct PartPrimitive {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Option<Vec<[f32; 2]>>,
    pub indices: Vec<u32>,
    /// Index into the `materials` slice passed to [`write_glb`].
    pub material: usize,
}

/// One named mesh node in the output file.
pub struct PartNode {
    pub name: String,
    pub primitives: Vec<PartPrimitive>,
}

/// Flat PBR material (Sketchfab gun parts are untextured).
#[derive(Clone, Debug)]
pub struct OutMaterial {
    pub name: Option<String>,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub double_sided: bool,
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

struct Packer {
    buffer: Vec<u8>,
    views: Vec<buffer::View>,
    accessors: Vec<accessor::Accessor>,
}

impl Packer {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            views: Vec::new(),
            accessors: Vec::new(),
        }
    }

    fn push_view(&mut self, data: &[u8]) -> Index<buffer::View> {
        let offset = align4(self.buffer.len());
        self.buffer.resize(offset, 0);
        self.buffer.extend_from_slice(data);
        let idx = self.views.len() as u32;
        self.views.push(buffer::View {
            buffer: Index::new(0),
            byte_length: data.len().into(),
            byte_offset: Some(offset.into()),
            byte_stride: None,
            extensions: None,
            extras: Default::default(),
            name: None,
            target: None,
        });
        Index::new(idx)
    }

    fn push_f32_vec3(&mut self, values: &[[f32; 3]]) -> Index<accessor::Accessor> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut bytes = Vec::with_capacity(values.len() * 12);
        for v in values {
            for k in 0..3 {
                min[k] = min[k].min(v[k]);
                max[k] = max[k].max(v[k]);
                bytes.extend_from_slice(&v[k].to_le_bytes());
            }
        }
        let view = self.push_view(&bytes);
        self.push_accessor(
            view,
            values.len(),
            accessor::ComponentType::F32,
            accessor::Type::Vec3,
            Some(min.to_vec()),
            Some(max.to_vec()),
        )
    }

    fn push_f32_vec2(&mut self, values: &[[f32; 2]]) -> Index<accessor::Accessor> {
        let mut bytes = Vec::with_capacity(values.len() * 8);
        for v in values {
            bytes.extend_from_slice(&v[0].to_le_bytes());
            bytes.extend_from_slice(&v[1].to_le_bytes());
        }
        let view = self.push_view(&bytes);
        self.push_accessor(
            view,
            values.len(),
            accessor::ComponentType::F32,
            accessor::Type::Vec2,
            None,
            None,
        )
    }

    fn push_u32_scalar(&mut self, values: &[u32]) -> Index<accessor::Accessor> {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let view = self.push_view(&bytes);
        self.push_accessor(
            view,
            values.len(),
            accessor::ComponentType::U32,
            accessor::Type::Scalar,
            None,
            None,
        )
    }

    fn push_accessor(
        &mut self,
        view: Index<buffer::View>,
        count: usize,
        component: accessor::ComponentType,
        ty: accessor::Type,
        min: Option<Vec<f32>>,
        max: Option<Vec<f32>>,
    ) -> Index<accessor::Accessor> {
        let idx = self.accessors.len() as u32;
        self.accessors.push(accessor::Accessor {
            buffer_view: Some(view),
            byte_offset: None,
            count: count.into(),
            component_type: Checked::Valid(accessor::GenericComponentType(component)),
            extensions: None,
            extras: Default::default(),
            type_: Checked::Valid(ty),
            min: min.map(serde_json::Value::from),
            max: max.map(serde_json::Value::from),
            name: None,
            normalized: false,
            sparse: None,
        });
        Index::new(idx)
    }
}

/// Assemble a complete `.glb` file (JSON chunk + BIN chunk) from part nodes.
pub fn write_glb(nodes: &[PartNode], materials: &[OutMaterial]) -> Result<Vec<u8>, String> {
    if nodes.is_empty() {
        return Err("refusing to write a GLB with no nodes".to_string());
    }
    let mut packer = Packer::new();
    let mut out_meshes: Vec<mesh::Mesh> = Vec::new();
    let mut out_nodes: Vec<scene::Node> = Vec::new();

    for node in nodes {
        if node.primitives.is_empty() {
            return Err(format!("node '{}' has no primitives", node.name));
        }
        let mut primitives = Vec::new();
        for prim in &node.primitives {
            if prim.material >= materials.len() {
                return Err(format!(
                    "node '{}' references material {} but only {} exist",
                    node.name,
                    prim.material,
                    materials.len()
                ));
            }
            if prim.positions.is_empty() || prim.indices.is_empty() {
                return Err(format!("node '{}' has an empty primitive", node.name));
            }
            if prim.indices.len() % 3 != 0 {
                return Err(format!(
                    "node '{}' index count {} is not a multiple of 3",
                    node.name,
                    prim.indices.len()
                ));
            }
            if prim.normals.len() != prim.positions.len() {
                return Err(format!(
                    "node '{}' has {} positions but {} normals",
                    node.name,
                    prim.positions.len(),
                    prim.normals.len()
                ));
            }
            if let Some(uvs) = &prim.uvs {
                if uvs.len() != prim.positions.len() {
                    return Err(format!("node '{}' has mismatched UV count", node.name));
                }
            }
            let mut attributes = BTreeMap::new();
            attributes.insert(
                Checked::Valid(mesh::Semantic::Positions),
                packer.push_f32_vec3(&prim.positions),
            );
            attributes.insert(
                Checked::Valid(mesh::Semantic::Normals),
                packer.push_f32_vec3(&prim.normals),
            );
            if let Some(uvs) = &prim.uvs {
                attributes.insert(
                    Checked::Valid(mesh::Semantic::TexCoords(0)),
                    packer.push_f32_vec2(uvs),
                );
            }
            primitives.push(mesh::Primitive {
                attributes,
                extensions: None,
                extras: Default::default(),
                indices: Some(packer.push_u32_scalar(&prim.indices)),
                material: Some(Index::new(prim.material as u32)),
                mode: Checked::Valid(mesh::Mode::Triangles),
                targets: None,
            });
        }
        let mesh_idx = out_meshes.len() as u32;
        out_meshes.push(mesh::Mesh {
            extensions: None,
            extras: Default::default(),
            name: Some(node.name.clone()),
            primitives,
            weights: None,
        });
        out_nodes.push(scene::Node {
            camera: None,
            children: None,
            extensions: None,
            extras: Default::default(),
            matrix: None,
            mesh: Some(Index::new(mesh_idx)),
            name: Some(node.name.clone()),
            rotation: None,
            scale: None,
            skin: None,
            translation: None,
            weights: None,
        });
    }

    let out_materials: Vec<material::Material> = materials
        .iter()
        .map(|m| material::Material {
            name: m.name.clone(),
            pbr_metallic_roughness: material::PbrMetallicRoughness {
                base_color_factor: material::PbrBaseColorFactor(m.base_color),
                metallic_factor: material::StrengthFactor(m.metallic),
                roughness_factor: material::StrengthFactor(m.roughness),
                ..Default::default()
            },
            double_sided: m.double_sided,
            ..Default::default()
        })
        .collect();

    let node_indices: Vec<Index<scene::Node>> =
        (0..out_nodes.len()).map(|i| Index::new(i as u32)).collect();
    let root = Root {
        accessors: packer.accessors,
        buffers: vec![gltf_json::Buffer {
            byte_length: packer.buffer.len().into(),
            extensions: None,
            extras: Default::default(),
            name: None,
            uri: None,
        }],
        buffer_views: packer.views,
        materials: out_materials,
        meshes: out_meshes,
        nodes: out_nodes,
        scene: Some(Index::new(0)),
        scenes: vec![scene::Scene {
            extensions: None,
            extras: Default::default(),
            name: Some("SplitScene".to_string()),
            nodes: node_indices,
        }],
        asset: gltf_json::Asset {
            version: "2.0".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };

    let mut json = serde_json::to_vec(&root).map_err(|e| format!("JSON serialize: {e}"))?;
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    while packer.buffer.len() % 4 != 0 {
        packer.buffer.push(0);
    }

    let total = 12 + 8 + json.len() + 8 + packer.buffer.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(&0x46546C67u32.to_le_bytes()); // 'glTF'
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F534Au32.to_le_bytes()); // 'JSON'
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(packer.buffer.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E4942u32.to_le_bytes()); // 'BIN\0'
    glb.extend_from_slice(&packer.buffer);
    Ok(glb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad_node() -> PartNode {
        PartNode {
            name: "TestQuad".to_string(),
            primitives: vec![PartPrimitive {
                positions: vec![
                    [-1.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [-1.0, 1.0, 0.0],
                ],
                normals: vec![[0.0, 0.0, 1.0]; 4],
                uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
                indices: vec![0, 1, 2, 0, 2, 3],
                material: 0,
            }],
        }
    }

    fn test_material() -> OutMaterial {
        OutMaterial {
            name: Some("test_mat".to_string()),
            base_color: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.2,
            roughness: 0.8,
            double_sided: false,
        }
    }

    #[test]
    fn written_glb_round_trips_through_the_gltf_crate() {
        let bytes = write_glb(&[quad_node()], &[test_material()]).expect("write must succeed");
        // Magic + version + total length header.
        assert_eq!(&bytes[0..4], &[0x67, 0x6C, 0x54, 0x46]);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
            bytes.len()
        );

        let (doc, buffers, _) =
            gltf::import_slice(&bytes).expect("written GLB must re-import");
        let meshes: Vec<_> = doc.meshes().collect();
        assert_eq!(meshes.len(), 1);
        let prim = meshes[0].primitives().next().unwrap();
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
        assert_eq!(positions.len(), 4);
        assert!((positions[1][0] - 1.0).abs() < 1e-6);
        let normals: Vec<[f32; 3]> = reader.read_normals().unwrap().collect();
        assert_eq!(normals.len(), 4);
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .unwrap()
            .into_f32()
            .collect();
        assert_eq!(uvs.len(), 4);
        let indices: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        assert_eq!(indices, vec![0, 1, 2, 0, 2, 3]);
        assert_eq!(prim.material().index(), Some(0));
        assert_eq!(
            prim.material().name(),
            Some("test_mat"),
            "material name must survive the round trip"
        );
    }

    #[test]
    fn writer_rejects_garbage() {
        assert!(write_glb(&[], &[test_material()]).is_err());
        let mut bad = quad_node();
        bad.primitives[0].indices = vec![0, 1];
        assert!(write_glb(&[bad], &[test_material()]).is_err());
        let mut bad_mat = quad_node();
        bad_mat.primitives[0].material = 7;
        assert!(write_glb(&[bad_mat], &[test_material()]).is_err());
    }
}
