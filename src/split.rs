//! Gun GLB splitter: inspection, split recipes, part assignment, socket proposals.
//!
//! Sketchfab gun models arrive as one GLB with descriptively-named part
//! groups (`hk416 barrel_15`, `m16 mag 30rnd (stanag)_13`, ...) each holding
//! 1+ single-material meshes, plus brass/copper bullet meshes and the odd
//! loose prop. This module:
//!
//!   1. inspects a gun GLB into per-group records (meshes, materials, bounds),
//!   2. auto-drafts a [`SplitRecipe`] (group name -> [`GunPart`]) from
//!      ordered name/material rules — the user reviews and edits this file,
//!   3. splits each part's meshes into its own GLB (see [`crate::glb_writer`]),
//!   4. proposes socket coordinates from part bounding boxes plus draft
//!      attachment JSON stubs.
//!
//! CLI (wired in main.rs):
//!   --split-recipe <gun.glb> <recipe.json>
//!   --split-gun <gun.glb> <recipe.json> <out-dir>

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::glb_writer::{self, OutMaterial, PartNode, PartPrimitive};

/// Attachment slot a group of meshes belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GunPart {
    Receiver,
    Barrel,
    Muzzle,
    Optic,
    Stock,
    Magazine,
    Underbarrel,
    Other,
    /// Deliberately excluded (bullets, shells, loose props).
    Drop,
    /// No rule matched — the user must assign this group in the recipe.
    Review,
}

impl GunPart {
    /// Output directory for the part's GLB, relative to the split out-dir.
    /// `None` for the receiver (goes to `receivers/`) and Drop/Review.
    pub fn slot_dir(&self) -> Option<&'static str> {
        match self {
            GunPart::Receiver | GunPart::Drop | GunPart::Review => None,
            GunPart::Barrel => Some("barrel"),
            GunPart::Muzzle => Some("muzzle"),
            GunPart::Optic => Some("optic"),
            GunPart::Stock => Some("stock"),
            GunPart::Magazine => Some("magazine"),
            GunPart::Underbarrel => Some("underbarrel"),
            GunPart::Other => Some("side"),
        }
    }

    /// Socket type string used for compatibility matching.
    pub fn socket_type(&self) -> Option<&'static str> {
        match self {
            GunPart::Barrel => Some("barrel_mount"),
            GunPart::Muzzle => Some("muzzle_thread"),
            GunPart::Optic => Some("optic_rail"),
            GunPart::Stock => Some("stock_mount"),
            GunPart::Magazine => Some("mag_well"),
            GunPart::Underbarrel => Some("underbarrel_rail"),
            GunPart::Other => Some("side_rail"),
            GunPart::Receiver | GunPart::Drop | GunPart::Review => None,
        }
    }

    /// Slot key used in gun/attachment JSON.
    pub fn slot_key(&self) -> Option<&'static str> {
        match self {
            GunPart::Barrel => Some("barrel"),
            GunPart::Muzzle => Some("muzzle"),
            GunPart::Optic => Some("optic"),
            GunPart::Stock => Some("stock"),
            GunPart::Magazine => Some("magazine"),
            GunPart::Underbarrel => Some("underbarrel"),
            GunPart::Other => Some("side"),
            GunPart::Receiver | GunPart::Drop | GunPart::Review => None,
        }
    }
}

/// World-space axis-aligned bounding box.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        }
    }

    fn include(&mut self, p: [f32; 3]) {
        for k in 0..3 {
            self.min[k] = self.min[k].min(p[k]);
            self.max[k] = self.max[k].max(p[k]);
        }
    }

    fn union(&mut self, other: &Bounds) {
        for k in 0..3 {
            self.min[k] = self.min[k].min(other.min[k]);
            self.max[k] = self.max[k].max(other.max[k]);
        }
    }

    pub fn center(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) / 2.0,
            (self.min[1] + self.max[1]) / 2.0,
            (self.min[2] + self.max[2]) / 2.0,
        ]
    }

    pub fn extents(&self) -> [f32; 3] {
        [
            (self.max[0] - self.min[0]).max(0.0),
            (self.max[1] - self.min[1]).max(0.0),
            (self.max[2] - self.min[2]).max(0.0),
        ]
    }

    pub fn contains(&self, other: &Bounds) -> bool {
        (0..3).all(|k| self.min[k] <= other.min[k] && other.max[k] <= self.max[k])
    }
}

/// One named part group: the meshes under it, the materials they use, and
/// their combined world-space bounds.
pub struct GroupInfo {
    pub group: String,
    pub mesh_indices: Vec<usize>,
    pub materials: Vec<String>,
    pub bounds: Bounds,
}

/// "Object_12"-style auto-generated mesh node names carry no meaning — the
/// parent group name is the real part name.
fn is_mesh_node_name(name: &str) -> bool {
    name.starts_with("Object_")
        && !name["Object_".len()..].is_empty()
        && name["Object_".len()..].chars().all(|c| c.is_ascii_digit())
}

fn resolve_group(own: Option<&str>, parent: Option<&str>) -> String {
    let own = own.unwrap_or("");
    if own.is_empty() || is_mesh_node_name(own) {
        parent.unwrap_or(own).to_string()
    } else {
        own.to_string()
    }
}

/// Ordered auto-classification rules. First match wins; see the module docs
/// for the rationale behind the ordering.
fn auto_part(group: &str) -> GunPart {
    let g = group.to_lowercase();
    let tokens: Vec<&str> = g
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let has_token = |words: &[&str]| tokens.iter().any(|t| words.contains(t));

    // 1. Loose ammo / scene props: bare-number names ("55645_14", "919_5")
    // and caliber-like names ("5.56x45", ".50 bmg", "12/70", "9x19").
    let base = g
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .trim_end_matches('_')
        .trim();
    if !base.is_empty() && base.chars().all(|c| c.is_ascii_digit()) {
        return GunPart::Drop;
    }
    let compact: String = base.chars().filter(|c| *c != ' ').collect();
    // Trailing variant markers ("n1", "n2") are not part of the caliber.
    let trimmed_digits = compact.trim_end_matches(|c: char| c.is_ascii_digit());
    let core = if trimmed_digits.len() < compact.len() {
        trimmed_digits.trim_end_matches('n')
    } else {
        compact.as_str()
    };
    let core = core
        .trim_end_matches("mm")
        .trim_end_matches("bmg")
        .trim_end_matches("acp");
    if !core.is_empty()
        && core
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '_' || c == 'x' || c == '/')
    {
        return GunPart::Drop;
    }
    // 2. Small receiver parts whose names contain ammo-ish words must win
    // over the loose-ammo rule below ("shell deflector", "mag release").
    if g.contains("mag release") || g.contains("mag catch") || g.contains("magazine release") {
        return GunPart::Receiver;
    }
    if g.contains("mag lock") || g.contains("maglock") {
        return GunPart::Receiver;
    }
    if has_token(&["ejector", "extractor", "deflector"]) {
        return GunPart::Receiver;
    }
    // 3. Loose ammo by vocabulary, plus "<caliber> case" ("919 case_6").
    if has_token(&[
        "bullet", "bullets", "cartridge", "cartridges", "casing", "casings", "shell",
        "shells", "round", "rounds", "pellet", "pellets",
    ]) {
        return GunPart::Drop;
    }
    if has_token(&["case", "cases"]) && tokens.iter().any(|t| t.chars().all(|c| c.is_ascii_digit())) {
        return GunPart::Drop;
    }
    // 4. Magazines ("m16 mag 30rnd", "17rnd mag"). Mag releases handled above.
    if g.contains("mag") {
        return GunPart::Magazine;
    }
    // 4b. Clips, belts and round-counts are feed devices too ("30rnd",
    // "stripper clip", "ammo belt"). Ammo boxes are props, not parts.
    if g.contains("ammo box") {
        return GunPart::Drop;
    }
    if g.contains("clip") || g.contains("belt") || g.contains("rnd") {
        return GunPart::Magazine;
    }
    // 5. Barrels before muzzle devices ("threaded barrel" is a barrel).
    if g.contains("barrel") {
        return GunPart::Barrel;
    }
    // 6. Muzzle devices.
    for w in [
        "muzzle",
        "flash hider",
        "flash guard",
        "compensator",
        "suppressor",
        "silencer",
        "brake",
        "thread",
    ] {
        if g.contains(w) {
            return GunPart::Muzzle;
        }
    }
    // 7. Optics / sights (carry handles hold the rear sight and detach;
    // brand names match as substrings: "busnhell39_9" is one token).
    if has_token(&[
        "sight", "sights", "scope", "optic", "dot", "holo", "reflex", "reticle", "rmr", "sro",
        "acog", "lpvo", "pso",
    ]) || ["bushnell", "busnhell", "pso"].iter().any(|w| g.contains(w))
        || g.contains("carry handle")
    {
        return GunPart::Optic;
    }
    // 8. Stocks ("stoc" covers the "stock"/"stoc" typo variants;
    // "button" is excluded so push-buttons don't land here).
    for w in [
        "stoc",
        "buttpad",
        "butt plate",
        "butt",
        "stoock",
        "cheek",
        "buffer tube",
    ] {
        if g.contains(w) && !g.contains("button") {
            return GunPart::Stock;
        }
    }
    // 9. Underbarrel (grips that are accessories, not the pistol grip).
    for w in ["foregrip", "bipod", "launcher", "vertical"] {
        if g.contains(w) {
            return GunPart::Underbarrel;
        }
    }
    // 10. Handguards / grips / rails stay on the receiver.
    for w in ["handguard", "hand guard", "grip", "rail"] {
        if g.contains(w) {
            return GunPart::Receiver;
        }
    }
    // 11. Small internals ("trigga"/"selecotr" cover common typos).
    for w in [
        "bolt", "carrier", "charging", "charge handle", "trig", "selector", "selecotr", "safety",
        "hammer", "firing", "sear", "disconnector", "pin", "spring", "slide", "frame", "dust",
        "cover", "latch", "button", "lever", "screw", "plate", "stop", "release", "catch",
        "reload handle", "follower", "gas tube", "pump", "pomp", "forend",
    ] {
        if g.contains(w) {
            return GunPart::Receiver;
        }
    }
    // 11b. Selector/safety switches named by function ("mode", "safe").
    if has_token(&["mode", "safe", "safety"]) {
        return GunPart::Receiver;
    }
    // 12. Tactical "other" (lights, lasers).
    if has_token(&["light", "lights"])
        || ["laser", "flashlight", "torch", "lamp", "peq", "dbal"]
            .iter()
            .any(|w| g.contains(w))
    {
        return GunPart::Other;
    }
    // 13. Explicit receiver words ("upper"/"lower" receivers).
    for w in ["receiver", "chassis", "body", "upper", "lower"] {
        if g.contains(w) {
            return GunPart::Receiver;
        }
    }
    GunPart::Review
}

/// A reviewed, deterministic mapping of group name -> part. Auto-drafted by
/// `--split-recipe`; the user edits `groups` and re-runs `--split-gun`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SplitRecipe {
    pub gun_id: String,
    /// Material names treated as ammunition (dropped unless inside a
    /// magazine group, where in-mag rounds are kept).
    #[serde(default = "default_ammo_materials")]
    pub ammo_materials: Vec<String>,
    pub groups: BTreeMap<String, GunPart>,
}

fn default_ammo_materials() -> Vec<String> {
    vec!["brass".to_string(), "copper".to_string()]
}

/// Inspect a gun GLB: one [`GroupInfo`] per named part group.
pub fn inspect_gun(path: &Path) -> Result<(gltf::Document, Vec<gltf::buffer::Data>, Vec<GroupInfo>), String> {
    let (document, buffers, _) =
        gltf::import(path).map_err(|e| format!("failed to load {}: {e}", path.display()))?;

    // group name -> mesh indices + materials + bounds
    let mut order: Vec<String> = Vec::new();
    let mut meshes: HashMap<String, Vec<usize>> = HashMap::new();
    let mut materials: HashMap<String, Vec<String>> = HashMap::new();
    let mut bounds: HashMap<String, Bounds> = HashMap::new();

    for scene in document.scenes() {
        for root in scene.nodes() {
            inspect_node(
                &root,
                &nalgebra::Matrix4::identity(),
                None,
                &buffers,
                &mut order,
                &mut meshes,
                &mut materials,
                &mut bounds,
            );
        }
    }

    let groups = order
        .into_iter()
        .map(|group| GroupInfo {
            mesh_indices: meshes.remove(&group).unwrap_or_default(),
            materials: materials.remove(&group).unwrap_or_default(),
            bounds: bounds.remove(&group).unwrap_or(Bounds::empty()),
            group,
        })
        .collect();
    Ok((document, buffers, groups))
}

#[allow(clippy::too_many_arguments)]
fn inspect_node(
    node: &gltf::Node,
    parent_xform: &nalgebra::Matrix4<f32>,
    parent_group: Option<String>,
    buffers: &[gltf::buffer::Data],
    order: &mut Vec<String>,
    meshes: &mut HashMap<String, Vec<usize>>,
    materials: &mut HashMap<String, Vec<String>>,
    bounds: &mut HashMap<String, Bounds>,
) {
    let world = parent_xform * crate::local_matrix(node);
    let group = resolve_group(node.name(), parent_group.as_deref());

    if let Some(mesh) = node.mesh() {
        if !order.contains(&group) {
            order.push(group.clone());
        }
        meshes.entry(group.clone()).or_default().push(mesh.index());
        let entry_bounds = bounds.entry(group.clone()).or_insert_with(Bounds::empty);
        let entry_mats = materials.entry(group.clone()).or_default();
        for prim in mesh.primitives() {
            if let Some(name) = prim.material().name() {
                let name = name.to_lowercase();
                if !entry_mats.contains(&name) {
                    entry_mats.push(name);
                }
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            if let Some(positions) = reader.read_positions() {
                for pos in positions {
                    let v = world * nalgebra::Vector4::new(pos[0], pos[1], pos[2], 1.0);
                    entry_bounds.include([v.x, v.y, v.z]);
                }
            }
        }
    }

    for child in node.children() {
        inspect_node(
            &child,
            &world,
            Some(group.clone()),
            buffers,
            order,
            meshes,
            materials,
            bounds,
        );
    }
}

/// Draft a recipe from inspection results using [`auto_part`].
pub fn auto_recipe(gun_id: &str, groups: &[GroupInfo]) -> SplitRecipe {
    SplitRecipe {
        gun_id: gun_id.to_string(),
        ammo_materials: default_ammo_materials(),
        groups: groups
            .iter()
            .map(|g| (g.group.clone(), auto_part(&g.group)))
            .collect(),
    }
}

/// Final mesh-level assignment: group mapping plus the ammo-material rule
/// (brass/copper dropped unless the group is a magazine).
pub fn assign_parts(
    recipe: &SplitRecipe,
    groups: &[GroupInfo],
) -> HashMap<usize, GunPart> {
    let mut out = HashMap::new();
    for g in groups {
        let mut part = recipe.groups.get(&g.group).copied().unwrap_or(GunPart::Review);
        if part != GunPart::Magazine
            && g.materials
                .iter()
                .any(|m| recipe.ammo_materials.iter().any(|a| a == m))
        {
            part = GunPart::Drop;
        }
        for m in &g.mesh_indices {
            out.insert(*m, part);
        }
    }
    prune_showoff_mags(groups, &mut out);
    out
}

/// A magazine node candidate for the keep rule (used both at split time
/// on groups and post-hoc on shipped part files).
struct MagCandidate {
    name: String,
    bounds: Bounds,
    has_ammo_mats: bool,
}

/// Keep the highest top face (seated mag inserts upward); ties within 0.15
/// prefer loaded (visible rounds), else largest. Index into `cands`;
/// requires non-empty.
fn choose_kept_mag(cands: &[MagCandidate]) -> usize {
    const TIE_EPS: f32 = 0.15;
    let top = cands
        .iter()
        .map(|c| c.bounds.max[1])
        .fold(f32::NEG_INFINITY, f32::max);
    let contenders: Vec<usize> = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| top - c.bounds.max[1] <= TIE_EPS)
        .map(|(i, _)| i)
        .collect();
    if contenders.len() == 1 {
        return contenders[0];
    }
    if let Some(pos) = contenders
        .iter()
        .position(|i| cands[*i].has_ammo_mats)
    {
        return contenders[pos];
    }
    *contenders
        .iter()
        .max_by(|a, b| {
            let va = {
                let bnd = &cands[**a].bounds;
                (bnd.max[0] - bnd.min[0]) * (bnd.max[1] - bnd.min[1]) * (bnd.max[2] - bnd.min[2])
            };
            let vb = {
                let bnd = &cands[**b].bounds;
                (bnd.max[0] - bnd.min[0]) * (bnd.max[1] - bnd.min[1]) * (bnd.max[2] - bnd.min[2])
            };
            va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap()
}

/// Magazine show-off prune: source models often contain TWO mags — the
/// mounted one seated up in the magwell, plus a spare floating beside or
/// below the gun. Keeps one group per [`choose_kept_mag`], drops the rest.
/// Single groups always survive, so guns never go magless.
fn prune_showoff_mags(groups: &[GroupInfo], assignment: &mut HashMap<usize, GunPart>) {
    let mag_groups: Vec<&GroupInfo> = groups
        .iter()
        .filter(|g| {
            g.mesh_indices
                .first()
                .and_then(|m| assignment.get(m))
                .copied()
                .unwrap_or(GunPart::Review)
                == GunPart::Magazine
        })
        .collect();
    if mag_groups.len() < 2 {
        return;
    }
    let cands: Vec<MagCandidate> = mag_groups
        .iter()
        .map(|g| MagCandidate {
            name: g.group.clone(),
            bounds: g.bounds,
            has_ammo_mats: g
                .materials
                .iter()
                .any(|m| m == "brass" || m == "copper"),
        })
        .collect();
    let keep_name = cands[choose_kept_mag(&cands)].name.clone();
    for g in mag_groups {
        if g.group != keep_name {
            for m in &g.mesh_indices {
                assignment.insert(*m, GunPart::Drop);
            }
        }
    }
}

// ── Sockets ───────────────────────────────────────────────────────────────

/// A proposed socket: position + euler rotation in the gun's own model
/// frame, plus which heuristic produced it (for review).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SocketProposal {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub heuristic: String,
}

/// Draft sockets for the gun JSON: receiver-mounted sockets plus
/// barrel-provided sockets (the muzzle).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SocketsDraft {
    pub gun_id: String,
    pub frame_note: String,
    /// slot -> proposal, in the gun's model frame.
    pub sockets: BTreeMap<String, SocketProposal>,
    /// part slot -> socket -> proposal, in the gun's model frame
    /// (parts share the frame since transforms are baked on split).
    pub provides: BTreeMap<String, BTreeMap<String, SocketProposal>>,
}

/// Dominant horizontal (x/z) axis of a bounding box: (axis, extent).
fn dominant_horizontal(extents: [f32; 3]) -> usize {
    if extents[0] >= extents[2] {
        0
    } else {
        2
    }
}

fn prop(position: [f32; 3], heuristic: &str) -> SocketProposal {
    SocketProposal {
        position,
        rotation: [0.0, 0.0, 0.0],
        heuristic: heuristic.to_string(),
    }
}

/// Propose sockets from part bounds. All coordinates are in the gun's own
/// model frame; rotations are identity (parts are modeled in gun space, the
/// fine-tune lives in the attachment JSON).
pub fn propose_sockets(
    gun_id: &str,
    assignment: &HashMap<usize, GunPart>,
    groups: &[GroupInfo],
    have: &[GunPart],
) -> SocketsDraft {
    let mut part_bounds: HashMap<GunPart, Bounds> = HashMap::new();
    for g in groups {
        // A group is single-part except when the ammo-material rule split
        // its meshes; use the part of its first mesh.
        let part = g
            .mesh_indices
            .first()
            .and_then(|m| assignment.get(m))
            .copied()
            .unwrap_or(GunPart::Review);
        if part == GunPart::Drop || part == GunPart::Review {
            continue;
        }
        part_bounds
            .entry(part)
            .and_modify(|b| b.union(&g.bounds))
            .or_insert(g.bounds);
    }

    let mut sockets = BTreeMap::new();
    let mut provides: BTreeMap<String, BTreeMap<String, SocketProposal>> = BTreeMap::new();
    let has = |p: GunPart| have.contains(&p);

    if let Some(r) = part_bounds.get(&GunPart::Receiver) {
        let rc = r.center();
        let f = dominant_horizontal(r.extents());
        let o = 2 - f; // the other horizontal axis
        // Forward sign: receiver center -> barrel center along the axis.
        let fwd = if let Some(b) = part_bounds.get(&GunPart::Barrel) {
            if b.center()[f] >= rc[f] {
                1.0
            } else {
                -1.0
            }
        } else {
            1.0
        };
        let fwd_pos = if fwd > 0.0 { r.max[f] } else { r.min[f] };
        let rear_pos = if fwd > 0.0 { r.min[f] } else { r.max[f] };

        if has(GunPart::Barrel) {
            let mut p = rc;
            p[f] = fwd_pos;
            sockets.insert(
                "barrel".to_string(),
                prop(p, "receiver forward-face center (barrel interface)"),
            );
        }
        if has(GunPart::Optic) {
            let mut p = rc;
            p[1] = r.max[1];
            sockets.insert("optic".to_string(), prop(p, "receiver top-face center (rail)"));
        } else {
            // No split optic part, but railed guns still take optics: same
            // proposal, flagged for explicit verification.
            let mut p = rc;
            p[1] = r.max[1];
            sockets.insert(
                "optic".to_string(),
                prop(p, "receiver top-face center (NO split optic part — verify rail)"),
            );
        }
        if has(GunPart::Stock) {
            let mut p = rc;
            p[f] = rear_pos;
            sockets.insert("stock".to_string(), prop(p, "receiver rear-face center"));
        }
        if has(GunPart::Underbarrel) {
            let mut p = rc;
            p[1] = r.min[1];
            // Two-thirds toward the muzzle from the rear.
            p[f] = rear_pos + (fwd_pos - rear_pos) * 0.65;
            sockets.insert(
                "underbarrel".to_string(),
                prop(p, "receiver bottom, forward third (verify against handguard)"),
            );
        } else {
            // Same proposal without a split part, flagged for verification.
            let mut p = rc;
            p[1] = r.min[1];
            p[f] = rear_pos + (fwd_pos - rear_pos) * 0.65;
            sockets.insert(
                "underbarrel".to_string(),
                prop(
                    p,
                    "receiver bottom, forward third (NO split underbarrel part — verify rail)",
                ),
            );
        }
        if has(GunPart::Other) {
            let mut p = rc;
            p[o] = r.max[o];
            sockets.insert(
                "other".to_string(),
                prop(p, "receiver +side-face center (verify rail side)"),
            );
        }
        if has(GunPart::Magazine) {
            if let Some(m) = part_bounds.get(&GunPart::Magazine) {
                let mc = m.center();
                sockets.insert(
                    "magazine".to_string(),
                    prop(
                        [mc[0], m.max[1], mc[2]],
                        "magazine top-face center (magwell)",
                    ),
                );
            }
        }
        if has(GunPart::Barrel) && has(GunPart::Muzzle) {
            if let Some(b) = part_bounds.get(&GunPart::Barrel) {
                let bc = b.center();
                let tip = if fwd > 0.0 { b.max[f] } else { b.min[f] };
                let mut p = bc;
                p[f] = tip;
                provides
                    .entry("barrel".to_string())
                    .or_default()
                    .insert(
                        "muzzle".to_string(),
                        prop(p, "barrel forward-face center (muzzle threads)"),
                    );
            }
        }
        // No separate barrel part (e.g. AK): the muzzle brake still needs a
        // home, so the receiver provides the muzzle socket, derived from the
        // muzzle part's own rear face (where it meets the barrel stub).
        if !has(GunPart::Barrel) && has(GunPart::Muzzle) {
            if let Some(m) = part_bounds.get(&GunPart::Muzzle) {
                let mc = m.center();
                let rear = if fwd > 0.0 { m.min[f] } else { m.max[f] };
                let mut p = mc;
                p[f] = rear;
                sockets.insert(
                    "muzzle".to_string(),
                    prop(p, "muzzle-part rear-face center (no separate barrel part)"),
                );
            }
        }
    }

    SocketsDraft {
        gun_id: gun_id.to_string(),
        frame_note: "Coordinates are in the gun model's own frame (no reorientation). Rotations are identity; fine-tune in the attachment JSON.".to_string(),
        sockets,
        provides,
    }
}

/// Display name for a split part, derived from its group names (and
/// materials for wood furniture). IDs stay `{gun}_{part}`; names are human.
fn part_display_name(part: GunPart, groups: &[String], materials: &[String]) -> String {
    let g = groups.join(" ").to_lowercase();
    let has_wood = materials.iter().any(|m| m.contains("wood"));
    match part {
        GunPart::Barrel => {
            if g.contains("thread") {
                "Threaded Barrel".to_string()
            } else {
                "Standard Barrel".to_string()
            }
        }
        GunPart::Muzzle => {
            if g.contains("suppressor") || g.contains("silencer") {
                "Suppressor".to_string()
            } else if g.contains("brake") || g.contains("compensator") {
                "Muzzle Brake".to_string()
            } else if g.contains("flash") {
                "Flash Guard".to_string()
            } else if g.contains("nut") || g.contains("protector") {
                "Thread Protector".to_string()
            } else {
                "Muzzle Device".to_string()
            }
        }
        GunPart::Optic => {
            if g.contains("sight") {
                "Iron Sights".to_string()
            } else if g.contains("scope") {
                "Scope".to_string()
            } else if g.contains("dot")
                || g.contains("reflex")
                || g.contains("holo")
                || g.contains("rmr")
                || g.contains("sro")
            {
                "Red Dot Sight".to_string()
            } else if g.contains("acog") || g.contains("lpvo") {
                "Magnified Optic".to_string()
            } else {
                "Optic".to_string()
            }
        }
        GunPart::Stock => {
            if has_wood {
                "Wooden Stock".to_string()
            } else {
                "Standard Stock".to_string()
            }
        }
        GunPart::Magazine => magazine_name(&g),
        GunPart::Underbarrel => {
            if g.contains("bipod") {
                "Bipod".to_string()
            } else if g.contains("launcher") || g.contains("grenade") {
                "Grenade Launcher".to_string()
            } else {
                "Vertical Foregrip".to_string()
            }
        }
        GunPart::Other => {
            if g.contains("laser") || g.contains("peq") || g.contains("dbal") {
                "Laser Module".to_string()
            } else if g.contains("light")
                || g.contains("flashlight")
                || g.contains("torch")
                || g.contains("lamp")
            {
                "Tactical Flashlight".to_string()
            } else {
                "Rail Accessory".to_string()
            }
        }
        _ => format!("{part:?}"),
    }
}

/// "30rnd stanag" -> "30-Round STANAG Magazine"; unknown -> "Standard Magazine".
fn magazine_name(g: &str) -> String {
    let mut cap: Option<String> = None;
    for token in g.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        // Capacity rides on an rnd/round token ("30rnd"); bare numbers are
        // calibers or model numbers ("55645", "919"), never capacities.
        if !(token.contains("rnd") || token.contains("round")) {
            continue;
        }
        let digits: String = token.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && cap.is_none() {
            cap = Some(digits);
        }
    }
    let qual = if g.contains("stanag") {
        "STANAG "
    } else if g.contains("steel") {
        "Steel "
    } else if g.contains("polymer") {
        "Polymer "
    } else if g.contains("bakelite") {
        "Bakelite "
    } else if g.contains("drum") {
        "Drum "
    } else {
        ""
    };
    match cap {
        Some(n) => format!("{n}-Round {qual}Magazine"),
        None if qual.is_empty() => "Standard Magazine".to_string(),
        None => format!("{qual}Magazine"),
    }
}

/// Translation to rebase a part by: its plug-in socket position, so the
/// mount point lands at the origin. The receiver is the frame reference and
/// stays put. Without this, parts keep baked gun-frame coordinates AND get
/// placed at the socket at runtime — a double offset.
fn rebase_for_part(part: GunPart, draft: &SocketsDraft) -> Result<Option<[f32; 3]>, String> {
    if part == GunPart::Receiver {
        return Ok(None);
    }
    if part == GunPart::Muzzle {
        if let Some(p) = draft.provides.get("barrel").and_then(|m| m.get("muzzle")) {
            return Ok(Some(p.position));
        }
    }
    let slot = part
        .slot_key()
        .ok_or_else(|| format!("{part:?} has no slot"))?;
    draft
        .sockets
        .get(slot)
        .map(|s| Some(s.position))
        .ok_or_else(|| format!("no '{slot}' socket to rebase by"))
}

// ── CLI entry points ──────────────────────────────────────────────────────

fn gun_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("gun")
        .to_string()
}

/// `--split-recipe <gun.glb> <recipe.json>`: inspect + auto-draft a recipe.
/// `--inspect <glb>`: print per-group mesh counts, materials and
/// world-space bounds. Diagnosis aid for wrong-part-kept issues (mounted
/// vs show-off twins) and spare-part hunts in receiver files.
pub fn cmd_inspect(in_path: &str) -> Result<(), String> {
    let path = PathBuf::from(in_path);
    let (_, _, groups) = inspect_gun(&path)?;
    for g in &groups {
        let e = g.bounds.extents();
        println!(
            "{:44} meshes={:<3} mats=[{}] min=[{:.3},{:.3},{:.3}] max=[{:.3},{:.3},{:.3}] size=[{:.3},{:.3},{:.3}]",
            g.group,
            g.mesh_indices.len(),
            g.materials.join(","),
            g.bounds.min[0],
            g.bounds.min[1],
            g.bounds.min[2],
            g.bounds.max[0],
            g.bounds.max[1],
            g.bounds.max[2],
            e[0],
            e[1],
            e[2],
        );
    }
    Ok(())
}

pub fn cmd_recipe(in_path: &str, out_path: &str) -> Result<(), String> {
    let path = PathBuf::from(in_path);
    let (_, _, groups) = inspect_gun(&path)?;
    let recipe = auto_recipe(&gun_id_from_path(&path), &groups);
    // Per-group review aid: part verdict, mesh count, materials, bounds.
    for g in &groups {
        let part = recipe.groups.get(&g.group).copied().unwrap_or(GunPart::Review);
        eprintln!(
            "  {:42} {:10} meshes={:<3} mats=[{}] bounds=[{:.2},{:.2},{:.2}]-[{:.2},{:.2},{:.2}]",
            g.group,
            format!("{part:?}"),
            g.mesh_indices.len(),
            g.materials.join(","),
            g.bounds.min[0],
            g.bounds.min[1],
            g.bounds.min[2],
            g.bounds.max[0],
            g.bounds.max[1],
            g.bounds.max[2],
        );
    }
    let review: Vec<&str> = recipe
        .groups
        .iter()
        .filter(|(_, p)| **p == GunPart::Review)
        .map(|(g, _)| g.as_str())
        .collect();
    let json = serde_json::to_string_pretty(&recipe).map_err(|e| e.to_string())?;
    let out_path = PathBuf::from(out_path);
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    std::fs::write(&out_path, &json).map_err(|e| e.to_string())?;
    eprintln!(
        "Wrote recipe for {} groups ({} need review: {}) to {}",
        recipe.groups.len(),
        review.len(),
        review.join(", "),
        out_path.display()
    );
    Ok(())
}

fn read_source_materials(document: &gltf::Document) -> Vec<OutMaterial> {
    document
        .materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            OutMaterial {
                name: m.name().map(|s| s.to_string()),
                base_color: pbr.base_color_factor(),
                metallic: pbr.metallic_factor(),
                roughness: pbr.roughness_factor(),
                double_sided: m.double_sided(),
            }
        })
        .collect()
}

/// `--split-sockets <gun.glb> <recipe.json>`: print the full sockets
/// draft (including optic/underbarrel proposals for guns without split
/// parts) as JSON to stdout. Used to add sockets to guns without
/// re-splitting.
pub fn cmd_sockets(in_path: &str, recipe_path: &str) -> Result<(), String> {
    let path = PathBuf::from(in_path);
    let recipe_json =
        std::fs::read_to_string(recipe_path).map_err(|e| format!("read recipe: {e}"))?;
    let recipe: SplitRecipe =
        serde_json::from_str(&recipe_json).map_err(|e| format!("parse recipe: {e}"))?;
    let (_, _, groups) = inspect_gun(&path)?;
    let assignment = assign_parts(&recipe, &groups);
    let mut have: Vec<GunPart> = assignment.values().copied().collect();
    have.sort_by_key(|p| *p as u8);
    have.dedup();
    let draft = propose_sockets(&recipe.gun_id, &assignment, &groups, &have);
    println!(
        "{}",
        serde_json::to_string_pretty(&draft).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// One mesh node with baked (file-space) geometry, as found in a split
/// output file. Split outputs bake world transforms into verts, so
/// mesh-local positions are already in file space.
struct BakedNode {
    name: String,
    bounds: Bounds,
    has_ammo_mats: bool,
    primitives: Vec<PartPrimitive>,
}

/// Read every mesh node of a GLB with file-space geometry.
/// Nodes are named by group (parent-resolved, so source files with
/// `Object_*` mesh names still match by part group) with the mesh index
/// appended when a group owns several meshes.
fn read_baked_nodes(in_path: &str) -> Result<(Vec<OutMaterial>, Vec<BakedNode>), String> {
    let (document, buffers, _) =
        gltf::import(in_path).map_err(|e| format!("failed to load {in_path}: {e}"))?;
    let source_materials = read_source_materials(&document);
    // mesh index -> group name via the scene hierarchy.
    let mut mesh_groups: HashMap<usize, String> = HashMap::new();
    let mut mesh_xforms: HashMap<usize, nalgebra::Matrix4<f32>> = HashMap::new();
    for scene in document.scenes() {
        for root in scene.nodes() {
            collect_groups(&root, None, &mut mesh_groups);
            collect_xforms(&root, &nalgebra::Matrix4::identity(), &mut mesh_xforms);
        }
    }
    let mut nodes: Vec<BakedNode> = Vec::new();
    for mesh in document.meshes() {
        // Group name (parent-resolved); meshes of one group share it, so
        // prune/drop/extract keep working on both split outputs (already
        // group-named) and raw sources (`Object_*` mesh names).
        let node_name = mesh_groups
            .get(&mesh.index())
            .cloned()
            .unwrap_or_else(|| mesh.name().unwrap_or("mesh").to_string());
        // Bake the world transform: split outputs are pre-baked (identity
        // here, no-op) while raw sources carry node transforms.
        let world = mesh_xforms
            .get(&mesh.index())
            .copied()
            .unwrap_or(nalgebra::Matrix4::identity());
        let normal_mat = world
            .fixed_view::<3, 3>(0, 0)
            .into_owned()
            .try_inverse()
            .map(|m| m.transpose())
            .unwrap_or(nalgebra::Matrix3::identity());
        let mut bounds = Bounds::empty();
        let mut has_ammo = false;
        let mut prims = Vec::new();
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| "primitive has no positions".to_string())?
                .map(|p| {
                    let v = world * nalgebra::Vector4::new(p[0], p[1], p[2], 1.0);
                    [v.x, v.y, v.z]
                })
                .collect();
            for p in &positions {
                bounds.include(*p);
            }
            let mat_idx = prim.material().index().unwrap_or(0);
            if source_materials.get(mat_idx).is_none() {
                return Err(format!("material {mat_idx} out of range"));
            }
            if let Some(name) = prim.material().name() {
                let n = name.to_lowercase();
                if n == "brass" || n == "copper" {
                    has_ammo = true;
                }
            }
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|it| {
                    it.map(|n| {
                        let v = normal_mat * nalgebra::Vector3::new(n[0], n[1], n[2]);
                        let l = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt().max(1e-9);
                        [v.x / l, v.y / l, v.z / l]
                    })
                    .collect()
                })
                .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|it| it.into_f32().collect());
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|it| it.into_u32().collect())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());
            prims.push(PartPrimitive {
                positions,
                normals,
                uvs,
                indices,
                material: mat_idx,
            });
        }
        nodes.push(BakedNode {
            name: node_name,
            bounds,
            has_ammo_mats: has_ammo,
            primitives: prims,
        });
    }
    Ok((source_materials, nodes))
}

/// `--hash-parts <glb>...`: per-mesh shape hashes for the dedupe
/// workflow ("check the mesh instead of assuming identical").
///
/// Bakes world transforms, translates by -min (position-invariant),
/// quantizes to 1mm, sorts, and FNV-1a hashes the vertex set. Identical
/// meshes in different spots — mounted vs show-off twins, or the same
/// sculpt shipped with two guns — hash equal; different sculpts (the
/// WAC-47 banana mag vs a STANAG) do not. Prints one line per mesh:
/// `hash verts tris file mesh`.
pub fn cmd_hash_parts(paths: &[String]) -> Result<(), String> {
    for path in paths {
        let (document, buffers, _) =
            gltf::import(path).map_err(|e| format!("failed to load {path}: {e}"))?;
        let mut xforms: HashMap<usize, nalgebra::Matrix4<f32>> = HashMap::new();
        for scene in document.scenes() {
            for root in scene.nodes() {
                collect_xforms(&root, &nalgebra::Matrix4::identity(), &mut xforms);
            }
        }
        for mesh in document.meshes() {
            let world = xforms
                .get(&mesh.index())
                .copied()
                .unwrap_or(nalgebra::Matrix4::identity());
            let mut positions: Vec<[f32; 3]> = Vec::new();
            let mut tris = 0usize;
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| Some(&buffers[b.index()]));
                let mut count = 0usize;
                if let Some(pos) = reader.read_positions() {
                    for p in pos {
                        let v =
                            world * nalgebra::Vector4::new(p[0], p[1], p[2], 1.0);
                        positions.push([v.x, v.y, v.z]);
                        count += 1;
                    }
                }
                tris += match reader.read_indices() {
                    Some(it) => it.into_u32().count() / 3,
                    None => count / 3,
                };
            }
            if positions.is_empty() {
                continue;
            }
            let mut min = [f32::INFINITY; 3];
            for p in &positions {
                for k in 0..3 {
                    min[k] = min[k].min(p[k]);
                }
            }
            let mut q: Vec<[i32; 3]> = positions
                .iter()
                .map(|p| {
                    [
                        ((p[0] - min[0]) * 1000.0).round() as i32,
                        ((p[1] - min[1]) * 1000.0).round() as i32,
                        ((p[2] - min[2]) * 1000.0).round() as i32,
                    ]
                })
                .collect();
            q.sort_unstable();
            let mut h: u64 = 0xcbf29ce484222325;
            for v in &q {
                for k in 0..3 {
                    for b in v[k].to_le_bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                }
            }
            println!(
                "{:016x} verts={:<6} tris={:<6} {path} {}",
                h,
                positions.len(),
                tris,
                mesh.name().unwrap_or("mesh"),
            );
        }
    }
    Ok(())
}

/// `--append-nodes <base.glb> <donor.glb> <out.glb>`: concat donor nodes
/// onto base (donor materials remap after base's). Restores parts lost to
/// misclassification without re-splitting (RPD-44 mag-lock lever was
/// dropped by the mag prune; it grafts back onto the receiver).
pub fn cmd_append_nodes(base: &str, donor: &str, out_path: &str) -> Result<(), String> {
    let (base_mats, base_nodes) = read_baked_nodes(base)?;
    let (donor_mats, donor_nodes) = read_baked_nodes(donor)?;
    let mut out_mats: Vec<OutMaterial> = Vec::new();
    let mut out_nodes: Vec<PartNode> = Vec::new();
    let mut push_prims = |prims: &[PartPrimitive],
                          src_mats: &[OutMaterial],
                          remap: &mut HashMap<usize, usize>,
                          out_mats: &mut Vec<OutMaterial>|
     -> Vec<PartPrimitive> {
        let mut out = Vec::new();
        for prim in prims {
            let remapped = *remap.entry(prim.material).or_insert_with(|| {
                let src = &src_mats[prim.material];
                out_mats.push(OutMaterial {
                    name: src.name.clone(),
                    base_color: src.base_color,
                    metallic: src.metallic,
                    roughness: src.roughness,
                    double_sided: src.double_sided,
                });
                out_mats.len() - 1
            });
            out.push(PartPrimitive {
                positions: prim.positions.clone(),
                normals: prim.normals.clone(),
                uvs: prim.uvs.clone(),
                indices: prim.indices.clone(),
                material: remapped,
            });
        }
        out
    };
    let mut remap: HashMap<usize, usize> = HashMap::new();
    for node in &base_nodes {
        let prims = push_prims(&node.primitives, &base_mats, &mut remap, &mut out_mats);
        out_nodes.push(PartNode {
            name: node.name.clone(),
            primitives: prims,
        });
    }
    let mut dremap: HashMap<usize, usize> = HashMap::new();
    for node in &donor_nodes {
        let prims = push_prims(&node.primitives, &donor_mats, &mut dremap, &mut out_mats);
        out_nodes.push(PartNode {
            name: node.name.clone(),
            primitives: prims,
        });
    }
    let bytes = glb_writer::write_glb(&out_nodes, &out_mats)?;
    std::fs::write(out_path, &bytes).map_err(|e| e.to_string())?;
    eprintln!(
        "appended {} donor node(s) onto {} base node(s)",
        donor_nodes.len(),
        base_nodes.len()
    );
    Ok(())
}

/// `--tri-report <glb>...`: per-file triangle totals plus per-mesh
/// breakdown, hottest first. Survey step of the iron-sight optimization
/// workflow: flag files over budget, then `--strip-small` the
/// multi-mesh ones or send single-mesh offenders back to the artist.
pub fn cmd_tri_report(paths: &[String]) -> Result<(), String> {
    let mut files: Vec<(String, usize, Vec<(String, usize)>)> = Vec::new();
    for path in paths {
        let (document, buffers, _) =
            gltf::import(path).map_err(|e| format!("failed to load {path}: {e}"))?;
        let mut meshes: Vec<(String, usize)> = Vec::new();
        for mesh in document.meshes() {
            let mut tris = 0usize;
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| Some(&buffers[b.index()]));
                let nverts = reader
                    .read_positions()
                    .map(|it| it.count())
                    .unwrap_or(0);
                tris += match reader.read_indices() {
                    Some(it) => it.into_u32().count() / 3,
                    None => nverts / 3,
                };
            }
            meshes.push((mesh.name().unwrap_or("mesh").to_string(), tris));
        }
        let total: usize = meshes.iter().map(|(_, t)| t).sum();
        files.push((path.clone(), total, meshes));
    }
    files.sort_by_key(|(_, t, _)| std::cmp::Reverse(*t));
    for (path, total, mut meshes) in files {
        println!("{total:>7} {path}");
        meshes.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        for (name, tris) in meshes {
            println!("          {tris:>7} {name}");
        }
    }
    Ok(())
}

/// `--strip-small <glb> <out.glb> <min_tris>`: drop meshes under the
/// triangle budget (screws, springs, pins modeled into iron sights).
/// Survivors keep byte-identical verts and names; materials remap.
/// Prints per-mesh keep/drop lines so the cut is reviewable.
pub fn cmd_strip_small(in_path: &str, out_path: &str, min_tris: usize) -> Result<(), String> {
    let (document, buffers, _) =
        gltf::import(in_path).map_err(|e| format!("failed to load {in_path}: {e}"))?;
    let source_materials = read_source_materials(&document);
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut out_mats: Vec<OutMaterial> = Vec::new();
    let mut out_nodes: Vec<PartNode> = Vec::new();
    let mut kept_tris = 0usize;
    let mut dropped_tris = 0usize;
    for mesh in document.meshes() {
        let name = mesh.name().unwrap_or("mesh").to_string();
        let mut prims: Vec<(PartPrimitive, usize)> = Vec::new();
        let mut tris = 0usize;
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| "primitive has no positions".to_string())?
                .collect();
            let nverts = positions.len();
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|it| it.into_u32().collect())
                .unwrap_or_else(|| (0..nverts as u32).collect());
            let ntris = indices.len() / 3;
            tris += ntris;
            let mat_idx = prim.material().index().unwrap_or(0);
            if source_materials.get(mat_idx).is_none() {
                return Err(format!("material {mat_idx} out of range"));
            }
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|it| it.collect())
                .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; nverts]);
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|it| it.into_f32().collect());
            prims.push((
                PartPrimitive {
                    positions,
                    normals,
                    uvs,
                    indices,
                    material: mat_idx,
                },
                ntris,
            ));
        }
        if tris < min_tris {
            eprintln!("drop {name} ({tris} tris)");
            dropped_tris += tris;
            continue;
        }
        eprintln!("keep {name} ({tris} tris)");
        kept_tris += tris;
        let mut out_prims = Vec::new();
        for (prim, _) in prims {
            let remapped = *remap.entry(prim.material).or_insert_with(|| {
                let src = &source_materials[prim.material];
                out_mats.push(OutMaterial {
                    name: src.name.clone(),
                    base_color: src.base_color,
                    metallic: src.metallic,
                    roughness: src.roughness,
                    double_sided: src.double_sided,
                });
                out_mats.len() - 1
            });
            out_prims.push(PartPrimitive {
                material: remapped,
                ..prim
            });
        }
        out_nodes.push(PartNode {
            name,
            primitives: out_prims,
        });
    }
    if out_nodes.is_empty() {
        return Err("strip-small would remove every mesh".to_string());
    }
    let bytes = glb_writer::write_glb(&out_nodes, &out_mats)?;
    std::fs::write(out_path, &bytes).map_err(|e| e.to_string())?;
    eprintln!("kept {kept_tris} tris, dropped {dropped_tris} tris");
    Ok(())
}

/// `--slice <glb> <axis:0,1,2> <lo,hi>`: per-group vert ranges inside an
/// axis slab. Measures well mouths: slice the receiver at the mag's x
/// span and read the body's lower surface instead of guessing sockets.
pub fn cmd_slice(in_path: &str, axis: usize, lo: f32, hi: f32) -> Result<(), String> {
    if axis > 2 {
        return Err("axis must be 0, 1 or 2".to_string());
    }
    let (document, buffers, _) =
        gltf::import(in_path).map_err(|e| format!("failed to load {in_path}: {e}"))?;
    let mut xforms: HashMap<usize, nalgebra::Matrix4<f32>> = HashMap::new();
    for scene in document.scenes() {
        for root in scene.nodes() {
            collect_xforms(&root, &nalgebra::Matrix4::identity(), &mut xforms);
        }
    }
    for mesh in document.meshes() {
        let world = xforms
            .get(&mesh.index())
            .copied()
            .unwrap_or(nalgebra::Matrix4::identity());
        let (a, b) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let mut n = 0usize;
        let (mut alo, mut ahi, mut blo, mut bhi) =
            (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
        for prim in mesh.primitives() {
            let reader = prim.reader(|bb| Some(&buffers[bb.index()]));
            if let Some(pos) = reader.read_positions() {
                for p in pos {
                    let v = world * nalgebra::Vector4::new(p[0], p[1], p[2], 1.0);
                    let c = [v.x, v.y, v.z];
                    if c[axis] < lo || c[axis] > hi {
                        continue;
                    }
                    n += 1;
                    alo = alo.min(c[a]);
                    ahi = ahi.max(c[a]);
                    blo = blo.min(c[b]);
                    bhi = bhi.max(c[b]);
                }
            }
        }
        if n > 0 {
            println!(
                "{:44} n={:<6} ax{}=[{:.3},{:.3}] ax{}=[{:.3},{:.3}]",
                mesh.name().unwrap_or("mesh"),
                n,
                a,
                alo,
                ahi,
                b,
                blo,
                bhi,
            );
        }
    }
    Ok(())
}

/// Mesh index -> part-group name via scene parents (same resolution as
/// inspection: `Object_*` mesh nodes inherit their parent group's name).
fn collect_groups(
    node: &gltf::Node,
    parent_group: Option<String>,
    out: &mut HashMap<usize, String>,
) {
    let group = resolve_group(node.name(), parent_group.as_deref());
    if let Some(mesh) = node.mesh() {
        out.insert(mesh.index(), group.clone());
    }
    for child in node.children() {
        collect_groups(&child, Some(group.clone()), out);
    }
}

/// `--drop-nodes <glb> <out.glb> <substr>...`: rewrite a split-output file
/// minus every node whose name contains one of the substrings. Survivors
/// keep byte-identical verts (no rebase); materials remap to survivors.
/// Used to cut show-off spares hiding in receiver files (Barrett M82,
/// Zbroyar Z-008) without re-splitting.
///
/// `--extract-nodes <glb> <out.glb> <substr>...`: the inverse — keep only
/// matching nodes. Used to promote a receiver-hiding spare to a real part
/// (the detailed twin becomes the mounted mag; the crude box retires).
pub fn cmd_drop_nodes(in_path: &str, out_path: &str, drops: &[String]) -> Result<(), String> {
    drop_or_extract(in_path, out_path, drops, false)
}

pub fn cmd_extract_nodes(
    in_path: &str,
    out_path: &str,
    keeps: &[String],
) -> Result<(), String> {
    drop_or_extract(in_path, out_path, keeps, true)
}

/// `--translate <glb> <out.glb> <dx,dy,dz>`: shift every vert. Used to
/// relocate a kept twin onto the well when the socket must stay put
/// (P90: the rounds-free twin seats at the existing socket).
pub fn cmd_translate(in_path: &str, out_path: &str, delta: [f32; 3]) -> Result<(), String> {
    let (source_materials, nodes) = read_baked_nodes(in_path)?;
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut out_mats: Vec<OutMaterial> = Vec::new();
    let mut out_nodes: Vec<PartNode> = Vec::new();
    for node in &nodes {
        let mut prims = Vec::new();
        for prim in &node.primitives {
            let remapped = *remap.entry(prim.material).or_insert_with(|| {
                let src = &source_materials[prim.material];
                out_mats.push(OutMaterial {
                    name: src.name.clone(),
                    base_color: src.base_color,
                    metallic: src.metallic,
                    roughness: src.roughness,
                    double_sided: src.double_sided,
                });
                out_mats.len() - 1
            });
            prims.push(PartPrimitive {
                positions: prim
                    .positions
                    .iter()
                    .map(|p| [p[0] + delta[0], p[1] + delta[1], p[2] + delta[2]])
                    .collect(),
                normals: prim.normals.clone(),
                uvs: prim.uvs.clone(),
                indices: prim.indices.clone(),
                material: remapped,
            });
        }
        out_nodes.push(PartNode {
            name: node.name.clone(),
            primitives: prims,
        });
    }
    let bytes = glb_writer::write_glb(&out_nodes, &out_mats)?;
    std::fs::write(out_path, &bytes).map_err(|e| e.to_string())?;
    eprintln!("translated {} node(s) by {delta:?}", out_nodes.len());
    Ok(())
}

fn drop_or_extract(
    in_path: &str,
    out_path: &str,
    patterns: &[String],
    invert: bool,
) -> Result<(), String> {
    let (source_materials, nodes) = read_baked_nodes(in_path)?;
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut out_mats: Vec<OutMaterial> = Vec::new();
    let mut out_nodes: Vec<PartNode> = Vec::new();
    let mut dropped = 0usize;
    for node in &nodes {
        let matched = patterns.iter().any(|d| node.name.contains(d));
        // Drop mode removes matches; extract mode removes non-matches.
        if matched != invert {
            eprintln!("removing node: {}", node.name);
            dropped += 1;
            continue;
        }
        let mut prims = Vec::new();
        for prim in &node.primitives {
            let remapped = *remap.entry(prim.material).or_insert_with(|| {
                let src = &source_materials[prim.material];
                out_mats.push(OutMaterial {
                    name: src.name.clone(),
                    base_color: src.base_color,
                    metallic: src.metallic,
                    roughness: src.roughness,
                    double_sided: src.double_sided,
                });
                out_mats.len() - 1
            });
            prims.push(PartPrimitive {
                positions: prim.positions.clone(),
                normals: prim.normals.clone(),
                uvs: prim.uvs.clone(),
                indices: prim.indices.clone(),
                material: remapped,
            });
        }
        out_nodes.push(PartNode {
            name: node.name.clone(),
            primitives: prims,
        });
    }
    if out_nodes.is_empty() {
        return Err("no node survived the drop/extract list".to_string());
    }
    let bytes = glb_writer::write_glb(&out_nodes, &out_mats)?;
    std::fs::write(out_path, &bytes).map_err(|e| e.to_string())?;
    eprintln!("dropped {dropped} node(s), kept {}", out_nodes.len());
    Ok(())
}

/// `--prune-mag <mag.glb> <out.glb> <old-socket: x,y,z>`: drop show-off
/// mag nodes from an already-split magazine part file.
/// `--prune-mag <mag.glb> <out.glb> <old-socket> [keep-substr]`: with a
/// fourth arg, keep the group whose name contains it instead of the
/// heuristic ([`choose_kept_mag`]) — for cases reviewed as wrong keeps
/// (RPD-44, P90: the empty twin is the seated mag).
///
/// Current verts live in gun-minus-old-socket space. Keeps one node group
/// per [`choose_kept_mag`], translates it so its top-center (the mount
/// point) lands exactly at the origin, and returns the new gun-frame
/// socket (old-socket space + old socket). Single-group files pass through
/// with their mount re-derived (translation ~zero when already canonical).
/// Prints the new socket as JSON: `{"position": [...]}`.
pub fn cmd_prune_mag(
    in_path: &str,
    out_path: &str,
    old_socket: [f32; 3],
    keep_override: Option<&str>,
) -> Result<[f32; 3], String> {
    let (source_materials, nodes) = read_baked_nodes(in_path)?;
    if nodes.is_empty() {
        return Err("mag file has no meshes".to_string());
    }

    // Group meshes by node name (splitter names every node after its group,
    // so one group may own several nodes) and choose the kept group.
    let mut order: Vec<String> = Vec::new();
    let mut by_group: HashMap<String, (Bounds, bool, Vec<usize>)> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        let entry = by_group.entry(n.name.clone()).or_insert_with(|| {
            order.push(n.name.clone());
            (Bounds::empty(), false, Vec::new())
        });
        entry.0.union(&n.bounds);
        entry.1 = entry.1 || n.has_ammo_mats;
        entry.2.push(i);
    }
    let keep_idx = if let Some(want) = keep_override {
        order
            .iter()
            .position(|n| n.contains(want))
            .ok_or_else(|| format!("no mag group contains {want:?}"))?
    } else {
        let cands: Vec<MagCandidate> = order
            .iter()
            .map(|name| {
                let (bounds, has_ammo, _) = &by_group[name];
                MagCandidate {
                    name: name.clone(),
                    bounds: *bounds,
                    has_ammo_mats: *has_ammo,
                }
            })
            .collect();
        choose_kept_mag(&cands)
    };
    eprintln!("prune keeps group: {}", order[keep_idx]);
    let keep_name = &order[keep_idx];
    let (keep_bounds, _, keep_meshes) = &by_group[keep_name];
    // Mount point in current file space; gun-frame mount adds old socket.
    let bc = keep_bounds.center();
    let mount_file = [bc[0], keep_bounds.max[1], bc[2]];
    let new_socket = [
        mount_file[0] + old_socket[0],
        mount_file[1] + old_socket[1],
        mount_file[2] + old_socket[2],
    ];

    // Translate so the mount lands exactly at the origin.
    let (dx, dy, dz) = (mount_file[0], mount_file[1], mount_file[2]);
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut out_mats: Vec<OutMaterial> = Vec::new();
    let mut out_nodes: Vec<PartNode> = Vec::new();
    for mi in keep_meshes {
        let node = &nodes[*mi];
        let mut prims = Vec::new();
        for prim in &node.primitives {
            let remapped = *remap.entry(prim.material).or_insert_with(|| {
                let src = &source_materials[prim.material];
                out_mats.push(OutMaterial {
                    name: src.name.clone(),
                    base_color: src.base_color,
                    metallic: src.metallic,
                    roughness: src.roughness,
                    double_sided: src.double_sided,
                });
                out_mats.len() - 1
            });
            prims.push(PartPrimitive {
                positions: prim
                    .positions
                    .iter()
                    .map(|p| [p[0] - dx, p[1] - dy, p[2] - dz])
                    .collect(),
                normals: prim.normals.clone(),
                uvs: prim.uvs.clone(),
                indices: prim.indices.clone(),
                material: remapped,
            });
        }
        out_nodes.push(PartNode {
            name: node.name.clone(),
            primitives: prims,
        });
    }
    let bytes = glb_writer::write_glb(&out_nodes, &out_mats)?;
    std::fs::write(out_path, &bytes).map_err(|e| e.to_string())?;
    Ok(new_socket)
}

/// `--split-gun <gun.glb> <recipe.json> <out-dir>`: split parts + drafts.
pub fn cmd_split(in_path: &str, recipe_path: &str, out_dir: &str) -> Result<(), String> {
    let path = PathBuf::from(in_path);
    let recipe_json =
        std::fs::read_to_string(recipe_path).map_err(|e| format!("read recipe: {e}"))?;
    let recipe: SplitRecipe =
        serde_json::from_str(&recipe_json).map_err(|e| format!("parse recipe: {e}"))?;

    let (document, buffers, groups) = inspect_gun(&path)?;
    let assignment = assign_parts(&recipe, &groups);

    let review: Vec<&str> = recipe
        .groups
        .iter()
        .filter(|(_, p)| **p == GunPart::Review)
        .map(|(g, _)| g.as_str())
        .collect();
    if !review.is_empty() {
        return Err(format!(
            "recipe has {} unassigned group(s) — assign them first: {}",
            review.len(),
            review.join(", ")
        ));
    }

    // Collect baked primitives per part.
    let mut part_nodes: HashMap<GunPart, Vec<PartNode>> = HashMap::new();
    // mesh index -> (group, world transform); recompute transforms here.
    let mut mesh_groups: HashMap<usize, String> = HashMap::new();
    for g in &groups {
        for m in &g.mesh_indices {
            mesh_groups.insert(*m, g.group.clone());
        }
    }
    let mut mesh_xforms: HashMap<usize, nalgebra::Matrix4<f32>> = HashMap::new();
    for scene in document.scenes() {
        for root in scene.nodes() {
            collect_xforms(&root, &nalgebra::Matrix4::identity(), &mut mesh_xforms);
        }
    }

    for mesh in document.meshes() {
        let part = assignment.get(&mesh.index()).copied().unwrap_or(GunPart::Review);
        if part == GunPart::Drop || part == GunPart::Review {
            continue;
        }
        let world = mesh_xforms
            .get(&mesh.index())
            .copied()
            .unwrap_or(nalgebra::Matrix4::identity());
        // Normal matrix: inverse-transpose of the upper 3x3.
        let normal_mat = world
            .fixed_view::<3, 3>(0, 0)
            .into_owned()
            .try_inverse()
            .map(|m| m.transpose())
            .unwrap_or(nalgebra::Matrix3::identity());
        let mut primitives = Vec::new();
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                return Err(format!("mesh {} is not triangles", mesh.index()));
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| format!("mesh {} has no positions", mesh.index()))?
                .map(|p| {
                    let v = world * nalgebra::Vector4::new(p[0], p[1], p[2], 1.0);
                    [v.x, v.y, v.z]
                })
                .collect();
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .ok_or_else(|| format!("mesh {} has no normals", mesh.index()))?
                .map(|n| {
                    let v = normal_mat * nalgebra::Vector3::new(n[0], n[1], n[2]);
                    let l = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt().max(1e-9);
                    [v.x / l, v.y / l, v.z / l]
                })
                .collect();
            let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|it| it.into_f32().collect());
            let indices: Vec<u32> = reader
                .read_indices()
                .ok_or_else(|| format!("mesh {} is not indexed", mesh.index()))?
                .into_u32()
                .collect();
            let pbr = prim.material().pbr_metallic_roughness();
            if pbr.base_color_texture().is_some() || pbr.metallic_roughness_texture().is_some() {
                // Textured primitives (scope reticles, decals) can't go
                // through the flat-material writer: warn loudly and drop
                // just the primitive, keeping the rest of the group.
                eprintln!(
                    "WARNING: mesh {} uses a textured material — dropping that primitive",
                    mesh.index()
                );
                continue;
            }
            primitives.push(PartPrimitive {
                positions,
                normals,
                uvs,
                indices,
                material: prim.material().index().unwrap_or(0),
            });
        }
        let name = mesh_groups
            .get(&mesh.index())
            .cloned()
            .unwrap_or_else(|| format!("mesh_{}", mesh.index()));
        if primitives.is_empty() {
            eprintln!(
                "WARNING: mesh {} lost all primitives to textured-material drops",
                mesh.index()
            );
            continue;
        }
        part_nodes.entry(part).or_default().push(PartNode {
            name,
            primitives,
        });
    }

    // Remap material indices per part and write GLBs.
    let source_materials = read_source_materials(&document);
    let out = PathBuf::from(out_dir);
    let have_parts: Vec<GunPart> = part_nodes.keys().copied().collect();

    // Every assigned non-drop part must have surviving geometry; otherwise
    // the part silently vanishes from the outputs.
    {
        let mut assigned: Vec<GunPart> = assignment.values().copied().collect();
        assigned.sort_by_key(|p| *p as u8);
        assigned.dedup();
        for part in assigned {
            if part != GunPart::Drop
                && part != GunPart::Review
                && !part_nodes.contains_key(&part)
            {
                return Err(format!(
                    "{part:?} lost all geometry to textured-material drops"
                ));
            }
        }
    }

    // Sockets first: parts rebase by their plug-in socket, so the mount
    // point lands at the origin and the runtime only positions the entity
    // at the target gun's socket (no double offset, portable across guns).
    let draft = propose_sockets(&recipe.gun_id, &assignment, &groups, &have_parts);

    // Per-part display names from group names + materials.
    let mut part_names: HashMap<GunPart, String> = HashMap::new();
    let mut part_groups: HashMap<GunPart, (Vec<String>, Vec<String>)> = HashMap::new();
    for g in &groups {
        let part = g
            .mesh_indices
            .first()
            .and_then(|m| assignment.get(m))
            .copied()
            .unwrap_or(GunPart::Review);
        if part == GunPart::Drop || part == GunPart::Review {
            continue;
        }
        let entry = part_groups.entry(part).or_insert_with(|| (Vec::new(), Vec::new()));
        entry.0.push(g.group.clone());
        for m in &g.materials {
            if !entry.1.contains(m) {
                entry.1.push(m.clone());
            }
        }
    }
    for (part, (names, mats)) in &part_groups {
        part_names.insert(*part, part_display_name(*part, names, mats));
    }

    let mut rebase: HashMap<GunPart, [f32; 3]> = HashMap::new();
    for part in &have_parts {
        if let Some(t) = rebase_for_part(*part, &draft)? {
            rebase.insert(*part, t);
        }
    }

    for (part, nodes) in &part_nodes {
        // Collect used material indices in first-seen order.
        let mut remap: HashMap<usize, usize> = HashMap::new();
        let mut out_mats: Vec<OutMaterial> = Vec::new();
        for node in nodes {
            for prim in &node.primitives {
                if !remap.contains_key(&prim.material) {
                    let src = source_materials.get(prim.material).ok_or_else(|| {
                        format!("material {} out of range", prim.material)
                    })?;
                    remap.insert(prim.material, out_mats.len());
                    out_mats.push(OutMaterial {
                        name: src.name.clone(),
                        base_color: src.base_color,
                        metallic: src.metallic,
                        roughness: src.roughness,
                        double_sided: src.double_sided,
                    });
                }
            }
        }
        let mut owned: Vec<PartNode> = Vec::new();
        let rebase_t = rebase.get(part).copied().unwrap_or([0.0, 0.0, 0.0]);
        for node in nodes {
            let mut prims = Vec::new();
            for prim in &node.primitives {
                prims.push(PartPrimitive {
                    positions: prim
                        .positions
                        .iter()
                        .map(|p| [p[0] - rebase_t[0], p[1] - rebase_t[1], p[2] - rebase_t[2]])
                        .collect(),
                    normals: prim.normals.clone(),
                    uvs: prim.uvs.clone(),
                    indices: prim.indices.clone(),
                    material: remap[&prim.material],
                });
            }
            owned.push(PartNode {
                name: node.name.clone(),
                primitives: prims,
            });
        }
        let bytes = glb_writer::write_glb(&owned, &out_mats)?;
        let rel = part_output_path(&recipe.gun_id, *part);
        let dest = out.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
        if rebase.contains_key(part) {
            eprintln!("Wrote {} ({} nodes, rebased)", dest.display(), owned.len());
        } else {
            eprintln!("Wrote {} ({} nodes)", dest.display(), owned.len());
        }
    }

    // Socket + attachment drafts.
    let mut whole = Bounds::empty();
    for g in &groups {
        whole.union(&g.bounds);
    }
    for (slot, s) in draft
        .sockets
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .chain(
            draft
                .provides
                .values()
                .flat_map(|m| m.values())
                .map(|s| ("(provided)", s)),
        )
    {
        if !whole.contains(&Bounds { min: s.position, max: s.position }) {
            eprintln!(
                "WARNING: socket {slot} at {:?} is outside the gun — check it in-game",
                s.position
            );
        }
    }
    let draft_json = serde_json::to_string_pretty(&draft).map_err(|e| e.to_string())?;
    let draft_path = out.join(format!("{}_sockets.json", recipe.gun_id));
    std::fs::write(&draft_path, &draft_json).map_err(|e| e.to_string())?;
    eprintln!("Wrote {}", draft_path.display());

    for part in &have_parts {
        if part.slot_dir().is_none() {
            continue;
        }
        let name = part_names
            .get(part)
            .cloned()
            .unwrap_or_else(|| format!("{part:?}"));
        let stub = attachment_stub(&recipe.gun_id, *part, &name, &draft);
        let stub_json = serde_json::to_string_pretty(&stub).map_err(|e| e.to_string())?;
        let stub_path = out.join(format!(
            "attachment_json/{}/{}_{}.json",
            part.slot_dir().unwrap(),
            recipe.gun_id,
            part.slot_key().unwrap()
        ));
        if let Some(parent) = stub_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&stub_path, &stub_json).map_err(|e| e.to_string())?;
    }
    eprintln!("Wrote {} attachment stubs", have_parts.iter().filter(|p| p.slot_dir().is_some()).count());
    Ok(())
}

fn part_output_path(gun_id: &str, part: GunPart) -> String {
    match part.slot_dir() {
        None => format!("receivers/{gun_id}.glb"),
        Some(slot) => format!(
            "attachments/{slot}/{gun_id}_{}.glb",
            part.slot_key().unwrap()
        ),
    }
}

/// `--split-prims <glb> <out-dir> [names...]`: explode every primitive of
/// every mesh into its own single-node GLB. Unlike `--split-gun` (which
/// groups by node/mesh), this splits *within* a mesh — for models whose
/// "parts" are material islands sharing one mesh (a crate whose body, lid
/// and latches are three primitives of one `Plane`). Prints per-part bounds
/// so parts can be identified, and uses the optional `names` in order.
pub fn cmd_split_prims(in_path: &str, out_dir: &str, names: &[String]) -> Result<(), String> {
    let (source_materials, nodes) = read_baked_nodes(in_path)?;
    let out = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    struct Part {
        name: String,
        material: OutMaterial,
        prim: PartPrimitive,
        bounds: Bounds,
    }

    let mut parts: Vec<Part> = Vec::new();
    let mut auto_idx = 0usize;
    for node in &nodes {
        let multi = node.primitives.len() > 1;
        for (i, prim) in node.primitives.iter().enumerate() {
            let name = if let Some(explicit) = names.get(parts.len()) {
                sanitize_part_name(explicit)
            } else if multi {
                sanitize_part_name(&format!("{}_{}", node.name, i))
            } else {
                sanitize_part_name(&node.name)
            };
            let mut bounds = Bounds::empty();
            for p in &prim.positions {
                bounds.include(*p);
            }
            let material = source_materials
                .get(prim.material)
                .cloned()
                .ok_or_else(|| format!("material {} out of range", prim.material))?;
            parts.push(Part {
                name,
                material,
                prim: PartPrimitive {
                    positions: prim.positions.clone(),
                    normals: prim.normals.clone(),
                    uvs: prim.uvs.clone(),
                    indices: prim.indices.clone(),
                    material: 0,
                },
                bounds,
            });
            auto_idx += 1;
        }
    }
    if parts.is_empty() {
        return Err(format!("{in_path} has no primitives to split"));
    }
    if !names.is_empty() && names.len() != parts.len() {
        return Err(format!(
            "got {} names for {} primitives",
            names.len(),
            parts.len()
        ));
    }

    for (i, part) in parts.iter().enumerate() {
        let size = part.bounds.extents();
        eprintln!(
            "[{i}] {:<20} tris={:<6} size=[{:.3},{:.3},{:.3}] min=[{:.3},{:.3},{:.3}] max=[{:.3},{:.3},{:.3}] mat={}",
            part.name,
            part.prim.indices.len() / 3,
            size[0],
            size[1],
            size[2],
            part.bounds.min[0],
            part.bounds.min[1],
            part.bounds.min[2],
            part.bounds.max[0],
            part.bounds.max[1],
            part.bounds.max[2],
            part.material.name.as_deref().unwrap_or("(unnamed)"),
        );
    }

    for part in &parts {
        let node = PartNode {
            name: part.name.clone(),
            primitives: vec![part.prim.clone()],
        };
        let bytes = glb_writer::write_glb(&[node], &[part.material.clone()])?;
        let dest = out.join(format!("{}.glb", part.name));
        std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
        eprintln!("Wrote {}", dest.display());
    }
    let _ = auto_idx;
    Ok(())
}

fn sanitize_part_name(name: &str) -> String {
    let mut out: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

/// Minimal draft attachment JSON: identity modifiers, mesh path filled in,
/// barrel provides the muzzle socket. Display name derived from the group
/// names; artist defaults to D_U (all current models).
fn attachment_stub(
    gun_id: &str,
    part: GunPart,
    name: &str,
    draft: &SocketsDraft,
) -> serde_json::Value {
    let slot = part.slot_key().unwrap();
    let id = format!("{gun_id}_{slot}");
    let mesh_path = format!(
        "weapons/models/attachments/{}/{}_{}.glb#Scene0",
        part.slot_dir().unwrap(),
        gun_id,
        slot
    );
    let provides = draft.provides.get(slot).cloned().unwrap_or_default();
    let provides_json: BTreeMap<String, serde_json::Value> = provides
        .into_iter()
        .map(|(sock, p)| {
            (
                sock,
                serde_json::json!({
                    "position": p.position,
                    "rotation": p.rotation,
                    "heuristic": p.heuristic,
                }),
            )
        })
        .collect();
    serde_json::json!({
        "id": id,
        "name": name,
        "artist": "D_U",
        "slot": slot,
        "socket_type": part.socket_type().unwrap(),
        "meta": {
            "mesh_path": mesh_path,
            "offset": [0.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0],
        },
        "provides_sockets": provides_json,
        "modifiers": {},
        "special_effects": [],
    })
}

fn collect_xforms(
    node: &gltf::Node,
    parent: &nalgebra::Matrix4<f32>,
    out: &mut HashMap<usize, nalgebra::Matrix4<f32>>,
) {
    let world = parent * crate::local_matrix(node);
    if let Some(mesh) = node.mesh() {
        out.insert(mesh.index(), world);
    }
    for child in node.children() {
        collect_xforms(&child, &world, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gun_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../noctyrn-game/assets/weapons/models")
            .join(name)
    }

    #[test]
    fn auto_rules_cover_the_hk416_groups() {
        let cases = [
            ("hk416 upper receiver_5", GunPart::Receiver),
            ("hk416 lower receiver_8", GunPart::Receiver),
            ("hk416 pistol grip_0", GunPart::Receiver),
            ("hk416 trigga_6", GunPart::Receiver),
            ("hk416 dust covet_9", GunPart::Receiver),
            ("hk416 handguard_4", GunPart::Receiver),
            ("hk416 charging handle_10", GunPart::Receiver),
            ("hk416 bolt carrier_18", GunPart::Receiver),
            ("hk416 bolt release_11", GunPart::Receiver),
            ("hk416 mag release_7", GunPart::Receiver),
            ("hk416 selector_1", GunPart::Receiver),
            ("hk416 barrel_15", GunPart::Barrel),
            ("hk416 flash guard_16", GunPart::Muzzle),
            ("hk416 front sight_2", GunPart::Optic),
            ("hk416 rare sight_17", GunPart::Optic),
            ("hk416 stock_3", GunPart::Stock),
            ("55645 m16 mag 30rnd (stanag)_13", GunPart::Magazine),
            ("55645 m16 mag empty 30rnd (stanag)_12", GunPart::Magazine),
            ("55645_14", GunPart::Drop),
            ("Object_4", GunPart::Review),
        ];
        for (group, expected) in cases {
            assert_eq!(auto_part(group), expected, "group {group}");
        }
    }

    #[test]
    fn auto_rules_cover_the_g17_groups() {
        let cases = [
            ("g17g5mos slide_0", GunPart::Receiver),
            ("g17 barrel threaded_1", GunPart::Barrel),
            ("g17 17rnd mag_2", GunPart::Magazine),
            ("g17g5 frame_3", GunPart::Receiver),
            ("g17 17rnd empty mag_4", GunPart::Magazine),
            ("919_5", GunPart::Drop),
            ("919 case_6", GunPart::Drop),
            ("g17 muzzle nut_7", GunPart::Muzzle),
            ("g17g5mos cover_8", GunPart::Receiver),
            ("g17 trigga_9", GunPart::Receiver),
            ("g17g5 slide stop_10", GunPart::Receiver),
        ];
        for (group, expected) in cases {
            assert_eq!(auto_part(group), expected, "group {group}");
        }
    }

    #[test]
    fn ak74_inspection_assigns_every_mesh_with_no_review() {
        // Reference full models live under models/primary/<category>/ now
        // that superseded full-gun GLBs are deprecated.
        let (_, _, groups) = inspect_gun(&gun_path("primary/assault/low-poly_ak-74.glb")).unwrap();
        let recipe = auto_recipe("ak-74", &groups);
        let review: Vec<_> = recipe
            .groups
            .iter()
            .filter(|(_, p)| **p == GunPart::Review)
            .collect();
        assert!(review.is_empty(), "ak-74 needs no manual review: {review:?}");

        let assignment = assign_parts(&recipe, &groups);
        let total: usize = groups.iter().map(|g| g.mesh_indices.len()).sum();
        assert_eq!(assignment.len(), total);
        let dropped: Vec<String> = assignment
            .iter()
            .filter(|(_, p)| **p == GunPart::Drop)
            .filter_map(|(m, _)| {
                groups
                    .iter()
                    .find(|g| g.mesh_indices.contains(m))
                    .map(|g| g.group.clone())
            })
            .collect();
        // Loose rounds plus the show-off empty twin drop; the seated
        // bakelite mag survives.
        for name in [
            "54539_10",
            "54539 case_11",
            "ak74 30rnd empty bakelite mag_13",
        ] {
            assert!(
                dropped.iter().any(|d| d == name),
                "{name} must drop, dropped={dropped:?}"
            );
        }
        assert!(
            !dropped.iter().any(|d| d == "ak74 30rnd bakelite mag_12"),
            "seated mag must survive, dropped={dropped:?}"
        );

        // The receiver must hold the core of the gun.
        let receiver_meshes = assignment
            .iter()
            .filter(|(_, p)| **p == GunPart::Receiver)
            .count();
        assert!(receiver_meshes >= 5, "receiver too small: {receiver_meshes}");
    }

    #[test]
    fn akdas_muzzle_gets_a_receiver_socket_without_a_barrel_part() {
        // The AKDAS has no separate barrel group, so the flash hider cannot
        // hang off a barrel-provided socket — the receiver must provide one.
        let (_, _, groups) =
            inspect_gun(&gun_path("primary/smg/low-poly_akdas_sa-9.glb")).unwrap();
        let recipe = auto_recipe("akdas_sa-9", &groups);
        let assignment = assign_parts(&recipe, &groups);
        let have: Vec<GunPart> = {
            let mut v: Vec<GunPart> = assignment.values().copied().collect();
            v.sort_by_key(|p| *p as u8);
            v.dedup();
            v
        };
        assert!(!have.contains(&GunPart::Barrel));
        assert!(have.contains(&GunPart::Muzzle));
        let draft = propose_sockets("akdas_sa-9", &assignment, &groups, &have);
        assert!(
            draft.sockets.contains_key("muzzle"),
            "receiver must provide the muzzle socket, got {:?}",
            draft.sockets.keys().collect::<Vec<_>>()
        );
        assert!(draft.sockets.contains_key("magazine"));
    }

    fn count_verts(path: &Path) -> (usize, usize) {
        let (doc, buffers, _) = gltf::import(path).unwrap();
        let mut meshes = 0;
        let mut verts = 0;
        for mesh in doc.meshes() {
            meshes += 1;
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| Some(&buffers[b.index()]));
                verts += reader.read_positions().map(|p| p.len()).unwrap_or(0);
            }
        }
        (meshes, verts)
    }

    #[test]
    fn end_to_end_split_vertex_totals_match() {
        let dir = std::env::temp_dir().join("noctyrn_split_test");
        let _ = std::fs::remove_dir_all(&dir);
        let gun = gun_path("primary/assault/low-poly_ak-74.glb");
        let recipe_path = dir.join("recipe.json");
        std::fs::create_dir_all(&dir).unwrap();
        cmd_recipe(gun.to_str().unwrap(), recipe_path.to_str().unwrap()).unwrap();
        let out = dir.join("out");
        cmd_split(
            gun.to_str().unwrap(),
            recipe_path.to_str().unwrap(),
            out.to_str().unwrap(),
        )
        .unwrap();

        // Source totals (minus drops).
        let (doc, buffers, gltf_groups) = inspect_gun(&gun).unwrap();
        let recipe: SplitRecipe =
            serde_json::from_str(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let assignment = assign_parts(&recipe, &gltf_groups);
        let mut kept = 0;
        for mesh in doc.meshes() {
            if assignment.get(&mesh.index()).copied().unwrap_or(GunPart::Drop) != GunPart::Drop {
                for prim in mesh.primitives() {
                    let reader = prim.reader(|b| Some(&buffers[b.index()]));
                    kept += reader.read_positions().map(|p| p.len()).unwrap_or(0);
                }
            }
        }

        // Every output file's verts must add up to the kept source verts,
        // whatever the part mix is.
        let mut out_verts = 0;
        let mut files = 0;
        let mut stack = vec![out.join("receivers"), out.join("attachments")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "glb") {
                    let (meshes, verts) = count_verts(&path);
                    assert!(meshes > 0, "{path:?} must hold nodes");
                    out_verts += verts;
                    files += 1;
                }
            }
        }
        assert!(files > 0, "split must emit part files");
        assert_eq!(out_verts, kept, "split must preserve every kept vertex");
        assert!(out.join("low-poly_ak-74_sockets.json").exists());
        // Rebase check: the magazine's mount point (its top face, the
        // magwell socket) must sit at the origin in its own file.
        let (mag_doc, mag_buffers, _) =
            gltf::import(out.join("attachments/magazine/low-poly_ak-74_magazine.glb")).unwrap();
        let mut mag_min = [f32::INFINITY; 3];
        let mut mag_max = [f32::NEG_INFINITY; 3];
        for mesh in mag_doc.meshes() {
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| Some(&mag_buffers[b.index()]));
                for p in reader.read_positions().unwrap() {
                    for k in 0..3 {
                        mag_min[k] = mag_min[k].min(p[k]);
                        mag_max[k] = mag_max[k].max(p[k]);
                    }
                }
            }
        }
        assert!(
            mag_max[1].abs() < 1e-4,
            "rebased mag top face must sit at y=0, got max {:?}",
            mag_max
        );
        let cx = (mag_min[0] + mag_max[0]) / 2.0;
        let cz = (mag_min[2] + mag_max[2]) / 2.0;
        assert!(
            cx.abs() < 1e-4 && cz.abs() < 1e-4,
            "rebased mag must be centered on x/z, got [{cx}, {cz}]"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn part_names_derive_from_group_names() {
        let g = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let no_mats: Vec<String> = Vec::new();
        let wood = vec!["wood_walnut".to_string()];
        let cases = [
            (GunPart::Barrel, "Standard Barrel", &["hk416 barrel_15"][..], &no_mats[..]),
            (GunPart::Barrel, "Threaded Barrel", &["g17 barrel threaded_1"], &no_mats[..]),
            (GunPart::Muzzle, "Flash Guard", &["hk416 flash guard_16"], &no_mats[..]),
            (GunPart::Muzzle, "Muzzle Brake", &["ak47 muzzle brake_3"], &no_mats[..]),
            (GunPart::Muzzle, "Thread Protector", &["g17 muzzle nut_7"], &no_mats[..]),
            (GunPart::Optic, "Iron Sights", &["hk416 front sight_2"], &no_mats[..]),
            (GunPart::Stock, "Standard Stock", &["hk416 stock_3"], &no_mats[..]),
            (GunPart::Stock, "Wooden Stock", &["ak stock_1"], &wood[..]),
            (GunPart::Magazine, "30-Round STANAG Magazine", &["55645 m16 mag 30rnd (stanag)_13"], &no_mats[..]),
            (GunPart::Magazine, "30-Round Steel Magazine", &["7.62x39 ak47 mag 30rnd (steel)_0"], &no_mats[..]),
            (GunPart::Magazine, "Standard Magazine", &["mystery mag_0"], &no_mats[..]),
        ];
        for (part, expected, groups, mats) in cases {
            assert_eq!(
                part_display_name(part, &g(groups), &mats.to_vec()),
                expected,
                "part {part:?}"
            );
        }
    }

    #[test]
    fn auto_rules_cover_loose_ammo_variants() {
        for loose in [
            "5.56x45_0",
            "5.56x45.001_11",
            "55645.001_12",
            "7.62x39_0",
            "7.62x39_13",
            "9x19_0",
            "919 _10",
            "919mm _0",
            ".45 acp_3",
            "45acp_1",
            ".50 bmg_0",
            "50bmg_1",
            "4.6x30_0",
            "12/70_0",
            "12/76_1",
            "12x70 n1_2",
            "1270n1_3",
        ] {
            assert_eq!(auto_part(loose), GunPart::Drop, "group {loose}");
        }
    }

    #[test]
    fn auto_rules_cover_feed_devices_and_small_parts() {
        // Upper/lower receivers (newer Sketchfab naming).
        assert_eq!(auto_part("m4a1 upper_0"), GunPart::Receiver);
        assert_eq!(auto_part("car15 lower_4"), GunPart::Receiver);        let cases = [
            ("76251 ar10 20rnd (scar)_11", GunPart::Magazine),
            ("mosin9130 clip_9", GunPart::Magazine),
            ("mosin9130 clip empty_10", GunPart::Magazine),
            ("76251 belt_3", GunPart::Magazine),
            ("m60 100rnd ammo box_4", GunPart::Drop),
            ("uzi charge handle_2", GunPart::Receiver),
            ("m1gar follower_5", GunPart::Receiver),
            ("rpd reload handle_13", GunPart::Receiver),
            ("rpk762 gas tube_8", GunPart::Receiver),
            ("saiga mode_4", GunPart::Receiver),
            ("p90 safe_4", GunPart::Receiver),
            ("rem870 pump forend_8", GunPart::Receiver),
            ("uzi stoock butt_8", GunPart::Stock),
            ("saiga stoc_3", GunPart::Stock),
            ("mpx selecotr_6", GunPart::Receiver),
            ("m10 silencer_14", GunPart::Muzzle),
            ("qms50 silencer_15", GunPart::Muzzle),
            ("pso-1_3", GunPart::Optic),
            ("busnhell39_9", GunPart::Optic),
            ("m4a1 carry handle_18", GunPart::Optic),
            // Regression guards: these must NOT match the new loose rules.
            ("ak-47_6", GunPart::Review),
        ];
        for (group, expected) in cases {
            assert_eq!(auto_part(group), expected, "group {group}");
        }
    }

    #[test]
    fn textured_reticle_drops_but_scope_survives() {
        // Tabuk's ON scope has a textured reticle mesh: the split must
        // succeed, keep the scope part, and drop only the textured primitive.
        let dir = std::env::temp_dir().join("noctyrn_split_tabuk_test");
        let _ = std::fs::remove_dir_all(&dir);
        let gun = gun_path("primary/dmr/low-poly_tabuk_sniper_rifle.glb");
        let recipe_path = dir.join("recipe.json");
        std::fs::create_dir_all(&dir).unwrap();
        cmd_recipe(gun.to_str().unwrap(), recipe_path.to_str().unwrap()).unwrap();
        let out = dir.join("out");
        cmd_split(
            gun.to_str().unwrap(),
            recipe_path.to_str().unwrap(),
            out.to_str().unwrap(),
        )
        .unwrap();
        let (doc, _, _) = inspect_gun(&out.join("attachments/optic/low-poly_tabuk_sniper_rifle_optic.glb"))
            .expect("scope GLB must exist");
        let meshes: Vec<_> = doc.meshes().collect();
        // Scope body + glass + mount survive; only the textured reticle
        // primitive is dropped (5 source meshes -> 4).
        assert_eq!(meshes.len(), 4, "scope keeps body + glass + mount");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn showoff_mag_dropped_mounted_kept() {
        // AK twins: the seated mag (at the mag release, top flush at the
        // well) survives and the ground-spare twin drops — whichever twin
        // is loaded. ak-74 keeps the loaded twin, akm keeps the empty one.
        for (gun, seated, spare) in [
            (
                "primary/assault/low-poly_ak-74.glb",
                "ak74 30rnd bakelite mag_12",
                "ak74 30rnd empty bakelite mag_13",
            ),
            (
                "primary/assault/low-poly_akm.glb",
                "ak 30rnd empty steel mag_14",
                "ak 30rnd steel mag_15",
            ),
        ] {
            let (_, _, groups) = inspect_gun(&gun_path(gun)).unwrap();
            let recipe = auto_recipe("test", &groups);
            let assignment = assign_parts(&recipe, &groups);
            let part_of = |name: &str| {
                groups
                    .iter()
                    .find(|g| g.group == name)
                    .and_then(|g| g.mesh_indices.first())
                    .and_then(|m| assignment.get(m))
                    .copied()
            };
            assert_eq!(
                part_of(seated),
                Some(GunPart::Magazine),
                "{gun}: seated mag must survive"
            );
            assert_eq!(
                part_of(spare),
                Some(GunPart::Drop),
                "{gun}: show-off mag must drop"
            );
        }
    }

    #[test]
    fn choose_kept_mag_prefers_highest_then_loaded() {
        let mk = |name: &str, max_y: f32, ammo: bool| MagCandidate {
            name: name.to_string(),
            bounds: Bounds {
                min: [-1.0, max_y - 1.0, -0.1],
                max: [1.0, max_y, 0.1],
            },
            has_ammo_mats: ammo,
        };
        // Highest top wins even without ammo mats.
        let cands = vec![mk("low", 0.0, true), mk("high", 0.5, false)];
        assert_eq!(choose_kept_mag(&cands), 1);
        // Tie within epsilon prefers loaded.
        let cands = vec![mk("plain", 0.5, false), mk("loaded", 0.45, true)];
        assert_eq!(choose_kept_mag(&cands), 1);
        // Tie without ammo prefers largest.
        let mut big = mk("big", 0.5, false);
        big.bounds.min[0] = -2.0;
        big.bounds.max[0] = 2.0;
        let cands = vec![mk("small", 0.5, false), big];
        assert_eq!(choose_kept_mag(&cands), 1);
        // Single candidate always survives.
        assert_eq!(choose_kept_mag(&[mk("only", -3.0, false)]), 0);
    }

    #[test]
    fn prune_mag_renders_mount_at_origin() {
        use crate::glb_writer::{OutMaterial, PartNode, PartPrimitive};
        // Two mag groups in gun-minus-old-socket space: a mounted box
        // (top y=0.1) and a show-off spare below (top y=-2.0). Old socket
        // (gun frame) sits at y=0.9, so gun-frame tops are 1.0 and -1.1.
        let boxy = |y0: f32, y1: f32, name: &str| PartNode {
            name: name.to_string(),
            primitives: vec![PartPrimitive {
                positions: vec![
                    [-1.0, y0, -0.1],
                    [1.0, y0, -0.1],
                    [1.0, y1, -0.1],
                    [-1.0, y1, -0.1],
                ],
                normals: vec![[0.0, 0.0, 1.0]; 4],
                uvs: None,
                indices: vec![0, 1, 2, 0, 2, 3],
                material: 0,
            }],
        };
        let mat = OutMaterial {
            name: Some("steel".to_string()),
            base_color: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.0,
            roughness: 0.8,
            double_sided: false,
        };
        let bytes = glb_writer::write_glb(
            &[boxy(-1.0, 0.1, "mounted"), boxy(-3.0, -2.0, "spare")],
            &[mat],
        )
        .unwrap();
        let dir = std::env::temp_dir().join("noctyrn_prune_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let inp = dir.join("mag.glb");
        let outp = dir.join("mag_out.glb");
        std::fs::write(&inp, &bytes).unwrap();

        let new_socket = cmd_prune_mag(
            inp.to_str().unwrap(),
            outp.to_str().unwrap(),
            [0.0, 0.9, 0.0],
            None,
        )
        .unwrap();
        // Gun-frame mount = file mount (0.1) + old socket (0.9).
        assert!((new_socket[1] - 1.0).abs() < 1e-4, "socket {new_socket:?}");
        assert!((new_socket[0]).abs() < 1e-4, "socket {new_socket:?}");

        // Output: single node, mount exactly at origin.
        let (doc, buffers, _) =
            gltf::import(outp.to_str().unwrap()).expect("pruned GLB must re-import");
        let meshes: Vec<_> = doc.meshes().collect();
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].name(), Some("mounted"));
        let prim = meshes[0].primitives().next().unwrap();
        let reader = prim.reader(|b| Some(&buffers[b.index()]));
        let ys: Vec<f32> = reader.read_positions().unwrap().map(|p| p[1]).collect();
        let top = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let xs: Vec<f32> = reader.read_positions().unwrap().map(|p| p[0]).collect();
        let cx = (xs.iter().cloned().fold(f32::INFINITY, f32::min)
            + xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max))
            / 2.0;
        assert!(top.abs() < 1e-4, "mount must sit at origin, top={top}");
        assert!(cx.abs() < 1e-4, "mount must sit at origin, cx={cx}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_small_drops_only_sub_budget_meshes() {
        use crate::glb_writer::{OutMaterial, PartNode, PartPrimitive};
        // One 4-tri mesh (kept) and one 1-tri mesh (dropped at min 2).
        let mesh = |ntris: u32, name: &str| {
            let nverts = ntris * 3;
            PartNode {
                name: name.to_string(),
                primitives: vec![PartPrimitive {
                    positions: (0..nverts).map(|i| [i as f32, 0.0, 0.0]).collect(),
                    normals: vec![[0.0, 1.0, 0.0]; nverts as usize],
                    uvs: None,
                    indices: (0..nverts).collect(),
                    material: 0,
                }],
            }
        };
        let mat = OutMaterial {
            name: Some("steel".to_string()),
            base_color: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.0,
            roughness: 0.8,
            double_sided: false,
        };
        let bytes =
            glb_writer::write_glb(&[mesh(4, "housing"), mesh(1, "screw")], &[mat]).unwrap();
        let dir = std::env::temp_dir().join("noctyrn_strip_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let inp = dir.join("part.glb");
        let outp = dir.join("part_out.glb");
        std::fs::write(&inp, &bytes).unwrap();
        cmd_strip_small(inp.to_str().unwrap(), outp.to_str().unwrap(), 2).unwrap();
        let (doc, _, _) =
            gltf::import(outp.to_str().unwrap()).expect("stripped GLB must re-import");
        let names: Vec<_> = doc
            .meshes()
            .map(|m| m.name().unwrap_or("").to_string())
            .collect();
        assert_eq!(names, vec!["housing"], "only the screw goes, got {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recipe_round_trips_through_json() {
        let recipe = SplitRecipe {
            gun_id: "test".to_string(),
            ammo_materials: default_ammo_materials(),
            groups: [("a".to_string(), GunPart::Barrel)].into_iter().collect(),
        };
        let json = serde_json::to_string_pretty(&recipe).unwrap();
        let back: SplitRecipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back.gun_id, "test");
        assert_eq!(back.groups["a"], GunPart::Barrel);
        assert_eq!(back.ammo_materials, vec!["brass", "copper"]);
    }

    #[test]
    fn socket_proposals_land_inside_the_gun() {
        let (_, _, groups) = inspect_gun(&gun_path("primary/assault/low-poly_ak-74.glb")).unwrap();
        let recipe = auto_recipe("ak-74", &groups);
        let assignment = assign_parts(&recipe, &groups);
        let mut whole = Bounds::empty();
        for g in &groups {
            whole.union(&g.bounds);
        }
        let have: Vec<GunPart> = {
            let mut v: Vec<GunPart> = assignment.values().copied().collect();
            v.sort_by_key(|p| *p as u8);
            v.dedup();
            v
        };
        let draft = propose_sockets("ak-74", &assignment, &groups, &have);
        assert!(draft.sockets.contains_key("barrel"), "need a barrel socket");
        assert!(draft.sockets.contains_key("optic"), "need an optic socket");
        assert!(draft.sockets.contains_key("magazine"), "need a magazine socket");
        assert!(draft.sockets.contains_key("stock"), "need a stock socket");
        assert!(
            draft.provides.get("barrel").is_some_and(|m| m.contains_key("muzzle")),
            "barrel must provide the muzzle socket"
        );
        for (slot, s) in draft
            .sockets
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .chain(
                draft.provides.values().flat_map(|m| m.values()).map(|s| ("(provided)", s)),
            )
        {
            assert!(
                whole.contains(&Bounds { min: s.position, max: s.position }),
                "socket {slot} at {:?} is outside the gun",
                s.position
            );
        }
    }
}
