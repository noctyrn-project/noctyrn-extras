//! Collider generation tool — one triangle mesh per mesh node.
//!
//! For each mesh node in the GLB, extracts triangle vertices and indices
//! in world space and writes them as JSON.
//! At runtime, parry3d builds a BVH-accelerated TriMesh for fast collision queries.
//!
//! Usage:
//!   cargo run --release -- --generate-mesh <path-in> <path-out>

use std::path::PathBuf;

mod glb_writer;
mod split;
mod texture;

#[derive(serde::Serialize)]
struct TriangleMesh {
    vertices: Vec<[f32; 3]>,
    indices: Vec<[u32; 3]>,
    /// The name of the first material used by this mesh node (if any).
    /// The game maps this name to a `MaterialType` at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    material: Option<String>,
}

#[derive(serde::Serialize)]
struct ColliderCollection {
    colliders: Vec<TriangleMesh>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  {} --generate-mesh <path-in> <path-out>", args[0]);
        eprintln!("  {} --split-recipe <gun.glb> <recipe.json>", args[0]);
        eprintln!("  {} --split-sockets <gun.glb> <recipe.json>", args[0]);
        eprintln!("  {} --prune-mag <mag.glb> <out.glb> <old-socket: x,y,z>", args[0]);
        eprintln!("  {} --inspect <glb>", args[0]);
        eprintln!("  {} --drop-nodes <glb> <out.glb> <substr>...", args[0]);
        eprintln!("  {} --extract-nodes <glb> <out.glb> <substr>...", args[0]);
        eprintln!("  {} --translate <glb> <out.glb> <dx,dy,dz>", args[0]);
        eprintln!("  {} --append-nodes <base.glb> <donor.glb> <out.glb>", args[0]);
        eprintln!("  {} --tri-report <glb>...", args[0]);
        eprintln!("  {} --strip-small <glb> <out.glb> <min_tris>", args[0]);
        eprintln!("  {} --slice <glb> <axis:0,1,2> <lo,hi>", args[0]);
        eprintln!("  {} --hash-parts <glb>...", args[0]);
        eprintln!("  {} --split-gun <gun.glb> <recipe.json> <out-dir>", args[0]);
        eprintln!("  {} --split-prims <glb> <out-dir> [name0,name1,...]", args[0]);
        eprintln!("  {} --gen-pattern <motif> <out.png> [size]", args[0]);
        std::process::exit(1);
    }

    let result = match args[1].as_str() {
        "--generate-mesh" => {
            if args.len() < 4 {
                eprintln!("Usage: {} --generate-mesh <path-in> <path-out>", args[0]);
                std::process::exit(1);
            }
            cmd_generate_mesh(&args[2], &args[3])
        }
        "--split-recipe" => {
            if args.len() < 4 {
                eprintln!("Usage: {} --split-recipe <gun.glb> <recipe.json>", args[0]);
                std::process::exit(1);
            }
            split::cmd_recipe(&args[2], &args[3])
        }
        "--split-sockets" => {
            if args.len() < 4 {
                eprintln!("Usage: {} --split-sockets <gun.glb> <recipe.json>", args[0]);
                std::process::exit(1);
            }
            split::cmd_sockets(&args[2], &args[3])
        }
        "--prune-mag" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --prune-mag <mag.glb> <out.glb> <old-socket: x,y,z>", args[0]);
                std::process::exit(1);
            }
            let coords: Vec<f32> = args[4]
                .split(',')
                .map(|s| {
                    s.trim().parse::<f32>().unwrap_or_else(|_| {
                        eprintln!("Bad socket coordinate: {s}");
                        std::process::exit(1);
                    })
                })
                .collect();
            if coords.len() != 3 {
                eprintln!("Socket needs exactly 3 coordinates: x,y,z");
                std::process::exit(1);
            }
            match split::cmd_prune_mag(&args[2], &args[3], [coords[0], coords[1], coords[2]], args.get(5).map(|s| s.as_str())) {
                Ok(new_socket) => {
                    println!(
                        "{}",
                        serde_json::json!({"position": new_socket})
                    );
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        "--split-gun" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --split-gun <gun.glb> <recipe.json> <out-dir>", args[0]);
                std::process::exit(1);
            }
            split::cmd_split(&args[2], &args[3], &args[4])
        }
        "--inspect" => {
            if args.len() < 3 {
                eprintln!("Usage: {} --inspect <glb>", args[0]);
                std::process::exit(1);
            }
            split::cmd_inspect(&args[2])
        }
        "--drop-nodes" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --drop-nodes <glb> <out.glb> <substr>...", args[0]);
                std::process::exit(1);
            }
            split::cmd_drop_nodes(&args[2], &args[3], &args[4..])
        }
        "--extract-nodes" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --extract-nodes <glb> <out.glb> <substr>...", args[0]);
                std::process::exit(1);
            }
            split::cmd_extract_nodes(&args[2], &args[3], &args[4..])
        }
        "--translate" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --translate <glb> <out.glb> <dx,dy,dz>", args[0]);
                std::process::exit(1);
            }
            let coords: Vec<f32> = args[4]
                .split(',')
                .map(|s| {
                    s.trim().parse::<f32>().unwrap_or_else(|_| {
                        eprintln!("Bad translate coordinate: {s}");
                        std::process::exit(1);
                    })
                })
                .collect();
            if coords.len() != 3 {
                eprintln!("Translate needs exactly 3 coordinates: dx,dy,dz");
                std::process::exit(1);
            }
            split::cmd_translate(&args[2], &args[3], [coords[0], coords[1], coords[2]])
        }
        "--append-nodes" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --append-nodes <base.glb> <donor.glb> <out.glb>", args[0]);
                std::process::exit(1);
            }
            split::cmd_append_nodes(&args[2], &args[3], &args[4])
        }
        "--tri-report" => {
            if args.len() < 3 {
                eprintln!("Usage: {} --tri-report <glb>...", args[0]);
                std::process::exit(1);
            }
            split::cmd_tri_report(&args[2..])
        }
        "--strip-small" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --strip-small <glb> <out.glb> <min_tris>", args[0]);
                std::process::exit(1);
            }
            let min: usize = args[4].trim().parse().unwrap_or_else(|_| {
                eprintln!("Bad min_tris: {}", args[4]);
                std::process::exit(1);
            });
            split::cmd_strip_small(&args[2], &args[3], min)
        }
        "--slice" => {
            if args.len() < 5 {
                eprintln!("Usage: {} --slice <glb> <axis:0,1,2> <lo,hi>", args[0]);
                std::process::exit(1);
            }
            let axis: usize = args[3].trim().parse().unwrap_or_else(|_| {
                eprintln!("Bad axis: {}", args[3]);
                std::process::exit(1);
            });
            let range: Vec<f32> = args[4]
                .split(',')
                .map(|s| {
                    s.trim().parse::<f32>().unwrap_or_else(|_| {
                        eprintln!("Bad range coordinate: {s}");
                        std::process::exit(1);
                    })
                })
                .collect();
            if range.len() != 2 {
                eprintln!("Range needs exactly 2 coordinates: lo,hi");
                std::process::exit(1);
            }
            split::cmd_slice(&args[2], axis, range[0], range[1])
        }
        "--split-prims" => {
            if args.len() < 4 {
                eprintln!("Usage: {} --split-prims <glb> <out-dir> [name0,name1,...]", args[0]);
                std::process::exit(1);
            }
            let names: Vec<String> = args
                .get(4)
                .map(|s| s.split(',').map(|n| n.trim().to_string()).collect())
                .unwrap_or_default();
            split::cmd_split_prims(&args[2], &args[3], &names)
        }
        "--gen-pattern" => {
            if args.len() < 4 {
                eprintln!("Usage: {} --gen-pattern <motif> <out.png> [size]", args[0]);
                std::process::exit(1);
            }
            let size: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2048);
            texture::cmd_gen_pattern(&args[2], &args[3], size)
        }
        "--hash-parts" => {
            if args.len() < 3 {
                eprintln!("Usage: {} --hash-parts <glb>...", args[0]);
                std::process::exit(1);
            }
            split::cmd_hash_parts(&args[2..])
        }
        other => {
            eprintln!("Unknown command: {other}");
            std::process::exit(1);
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_generate_mesh(path_in: &str, path_out: &str) -> Result<(), String> {
    let path_in = PathBuf::from(path_in);
    let path_out = PathBuf::from(path_out);

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
    std::fs::write(&path_out, &json).map_err(|e| e.to_string())?;
    eprintln!("Wrote {}", path_out.display());
    Ok(())
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
        // Material name of the first primitive — the runtime maps it to a
        // MaterialType (concrete/wood/glass/... / world for everything else).
        let material = mesh
            .primitives()
            .next()
            .and_then(|p| p.material().name())
            .map(|n| n.to_string());

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
            colliders.push(TriangleMesh {
                vertices,
                indices,
                material,
            });
        }
    }

    for child in node.children() {
        process_node(&child, &world, buffers, colliders);
    }
}

pub(crate) fn local_matrix(node: &gltf::Node) -> nalgebra::Matrix4<f32> {
    let (t, q, s) = node.transform().decomposed();
    trs_matrix(t, q, s)
}

/// Translation × rotation × scale matrix from glTF TRS components.
fn trs_matrix(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> nalgebra::Matrix4<f32> {
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
    // glTF applies scale in the node's LOCAL space first, then rotates:
    // T·R·S. Scale the rotation COLUMNS by the per-axis scale. (Scaling the
    // rows instead would compute S·R — the scale after the rotation — which
    // silently breaks rotated meshes with non-uniform scale, since the two
    // only agree when the scale is uniform.)
    for i in 0..3 {
        mat[(i, 0)] *= s[0];
        mat[(i, 1)] *= s[1];
        mat[(i, 2)] *= s[2];
    }
    mat
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The testing-grounds cone: rotation (0.22733, 0.18585, -0.19982,
    /// 0.93480), scale (2.6191, 1.7071, 2.6191), translation (3.2823,
    /// 2.0290, 41.4162). A local corner at (1, 1, 1) must land exactly
    /// where the rotation + scale + translation put it — if the rotation is
    /// dropped, the AABB center drifts away from the node's translation.
    #[test]
    fn rotated_scale_applies_in_world_space() {
        let t = [3.282339334487915, 2.029005289077759, 41.41621780395508];
        let q = [0.22732633352279663, 0.18585112690925598, -0.19982382655143738, 0.9348008036613464];
        let s = [2.6191329956054688, 1.707076072692871, 2.6191329956054688];
        let m = trs_matrix(t, q, s);
        let corner = m * nalgebra::Vector4::new(1.0, 1.0, 1.0, 1.0);
        let expected = nalgebra::Vector4::new(6.965, 1.358, 43.034, 1.0);
        let diff = (corner - expected).abs().max();
        assert!(
            diff < 0.01,
            "rotated corner mismatch: got {corner:?}, expected ~{expected:?}"
        );
    }

    #[test]
    fn pure_rotation_matches_axis_expectation() {
        // rotation_y(90°): q = (0, sin45, 0, cos45) maps +Z → +X.
        let m = trs_matrix([0.0, 0.0, 0.0], [0.0, 0.70710678, 0.0, 0.70710678], [1.0, 1.0, 1.0]);
        let z = m * nalgebra::Vector4::new(0.0, 0.0, 1.0, 1.0);
        assert!((z.x - 1.0).abs() < 1e-4 && z.y.abs() < 1e-4 && z.z.abs() < 1e-4,
            "rotation_y(90°) must map +Z to +X, got {z:?}");
    }

    /// End-to-end bake of the real testing-grounds GLB: the rotated,
    /// non-uniformly-scaled Cone must bake EXACTLY as its local vertices
    /// through the T·R·S matrix (this is the path that drifted before the
    /// column-scale fix).
    #[test]
    fn baked_rotated_mesh_round_trips_through_trs() {
        let path_in = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../noctyrn-game/assets/maps/testing_grounds.glb");
        let (document, buffers, _) = gltf::import(&path_in).unwrap();
        let mut colliders: Vec<TriangleMesh> = Vec::new();
        for scene in document.scenes() {
            for root in scene.nodes() {
                process_node(&root, &nalgebra::Matrix4::identity(), &buffers, &mut colliders);
            }
        }

        // The first scene node is the Cone.
        let node = document.scenes().next().unwrap().nodes().next().unwrap();
        let mesh = node.mesh().expect("cone node has a mesh");
        let (t, q, s) = node.transform().decomposed();
        let m = trs_matrix(t, q, s);
        let mut expected: Vec<[f32; 3]> = Vec::new();
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|b| Some(&buffers[b.index()]));
            if let Some(positions) = reader.read_positions() {
                for pos in positions {
                    let v = m * nalgebra::Vector4::new(pos[0], pos[1], pos[2], 1.0);
                    expected.push([v.x, v.y, v.z]);
                }
            }
        }

        let cone = &colliders[0];
        assert_eq!(
            cone.vertices.len(),
            expected.len(),
            "baked vertex count differs"
        );
        for (baked, manual) in cone.vertices.iter().zip(expected.iter()) {
            for k in 0..3 {
                assert!(
                    (baked[k] - manual[k]).abs() < 0.001,
                    "baked vertex {baked:?} != T·R·S of local {manual:?}"
                );
            }
        }
    }
}
