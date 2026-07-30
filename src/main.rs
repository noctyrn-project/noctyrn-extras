//! Collider generation tool — one triangle mesh per mesh node.
//!
//! For each mesh node in the GLB, extracts triangle vertices and indices
//! in world space and writes them as JSON.
//! At runtime, parry3d builds a BVH-accelerated TriMesh for fast collision queries.
//!
//! Usage:
//!   cargo run --release -- --generate-mesh <path-in> <path-out>

use std::path::PathBuf;

#[derive(serde::Serialize)]
struct TriangleMesh {
    vertices: Vec<[f32; 3]>,
    indices: Vec<[u32; 3]>,
}

#[derive(serde::Serialize)]
struct ColliderCollection {
    colliders: Vec<TriangleMesh>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args[1] != "--generate-mesh" {
        eprintln!("Usage: {} --generate-mesh <path-in> <path-out>", args[0]);
        std::process::exit(1);
    }

    let path_in = PathBuf::from(&args[2]);
    let path_out = PathBuf::from(&args[3]);

    let (document, buffers, _) = gltf::import(&path_in).unwrap_or_else(|e| {
        eprintln!("Failed to load GLB: {e}");
        std::process::exit(1);
    });

    let mut colliders: Vec<TriangleMesh> = Vec::new();

    for scene in document.scenes() {
        for root_node in scene.nodes() {
            process_node(&root_node, &nalgebra::Matrix4::identity(), &buffers, &mut colliders);
        }
    }

    eprintln!("Generated {} triangle meshes", colliders.len());
    let collection = ColliderCollection { colliders };
    let json = serde_json::to_string_pretty(&collection).unwrap();
    std::fs::write(&path_out, &json).unwrap();
    eprintln!("Wrote {}", path_out.display());
}

fn process_node(
    node: &gltf::Node,
    parent_xform: &nalgebra::Matrix4<f32>,
    buffers: &[gltf::buffer::Data],
    colliders: &mut Vec<TriangleMesh>,
) {
    let local = local_matrix(node);
    let world = parent_xform * local;

    if let Some(mesh) = node.mesh() {
        let mut vertices: Vec<[f32; 3]> = Vec::new();
        let mut indices: Vec<[u32; 3]> = Vec::new();

        for primitive in mesh.primitives() {
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
            if let Some(positions) = reader.read_positions() {
                let base = vertices.len() as u32;
                for pos in positions {
                    let t = world * nalgebra::Vector4::new(pos[0], pos[1], pos[2], 1.0);
                    vertices.push([t.x, t.y, t.z]);
                }

                if let Some(reader_indices) = reader.read_indices() {
                    let idx: Vec<u32> = reader_indices.into_u32().collect();
                    for chunk in idx.chunks(3) {
                        if chunk.len() == 3 {
                            indices.push([base + chunk[0], base + chunk[1], base + chunk[2]]);
                        }
                    }
                } else {
                    for i in (0..vertices.len() as u32 - base).step_by(3) {
                        indices.push([base + i, base + i + 1, base + i + 2]);
                    }
                }
            }
        }

        if !indices.is_empty() {
            colliders.push(TriangleMesh { vertices, indices });
        }
    }

    for child in node.children() {
        process_node(&child, &world, buffers, colliders);
    }
}

fn local_matrix(node: &gltf::Node) -> nalgebra::Matrix4<f32> {
    let (t, q, s) = node.transform().decomposed();
    let x = q[0]; let y = q[1]; let z = q[2]; let w = q[3];

    let mut mat = nalgebra::Matrix4::identity();
    mat[(0, 0)] = 1.0 - 2.0*y*y - 2.0*z*z;
    mat[(0, 1)] = 2.0*x*y - 2.0*z*w;
    mat[(0, 2)] = 2.0*x*z + 2.0*y*w;
    mat[(1, 0)] = 2.0*x*y + 2.0*z*w;
    mat[(1, 1)] = 1.0 - 2.0*x*x - 2.0*z*z;
    mat[(1, 2)] = 2.0*y*z - 2.0*x*w;
    mat[(2, 0)] = 2.0*x*z - 2.0*y*w;
    mat[(2, 1)] = 2.0*y*z + 2.0*x*w;
    mat[(2, 2)] = 1.0 - 2.0*x*x - 2.0*y*y;
    mat[(0, 3)] = t[0];
    mat[(1, 3)] = t[1];
    mat[(2, 3)] = t[2];
    for i in 0..3 {
        mat[(0, i)] *= s[0];
        mat[(1, i)] *= s[1];
        mat[(2, i)] *= s[2];
    }
    mat
}
