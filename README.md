# noctyrn-extras

Content pipeline tools for the Noctyrn game.

For the full weapon workflow (splitting, sockets, naming, JSON schemas,
in-game verification), see `../WEAPONS.md`.

## Commands

### `--generate-mesh <path-in> <path-out>`

Loads a GLB file and writes a JSON file containing AABB collider descriptors
suitable for `noctyrn_shared::map_data::ColliderCollection`.

The algorithm subdivides each mesh primitive into a voxel grid
(configurable via `SUBDIV` in `src/main.rs`) and outputs one AABB per
occupied grid cell. Adjacent cells are then merged along each axis to
reduce the total box count.

**Usage:**

    cargo run --release -- --generate-mesh \
        ../noctyrn-game/assets/maps/duststorm.glb \
        dust_storm_colliders.json

**Output format** — an object with a single `colliders` key:

```json
{
  "colliders": [
    {
      "center": [1.0, 2.0, 3.0],
      "half_extents": [0.5, 1.0, 0.5],
      "material": 0
    }
  ]
}
```

The `material` field is an integer material type:
- `0` = Concrete (default)
- `1` = Metal
- `2` = Wood
- `3` = Glass
- `4` = Drywall

### Limitations

- **Does not support Draco-compressed GLBs.**  Export uncompressed GLB from
  your DCC tool for collision generation.  (You can keep the Draco-compressed
  version in `assets/maps/` for runtime.)
- The voxel grid resolution (`SUBDIV`) is a global constant.  Very large
  or very small meshes may not be optimally subdivided.

## Gun splitter

Sketchfab gun models arrive as one GLB with descriptively-named part groups
(`hk416 barrel_15`, `m16 mag 30rnd (stanag)_13`, ...) plus brass/copper
bullet meshes and the odd loose prop. The splitter separates them into a
receiver GLB plus one GLB per attachment part — no Blender work required —
and proposes socket coordinates plus draft attachment JSONs.

**Usage (two steps — recipe first, so you can review before splitting):**

    # 1. Inspect the gun and auto-draft a recipe (group -> part mapping).
    cargo run --release -- --split-recipe \
        ../noctyrn-game/assets/weapons/models/primary/assault/hk416.glb \
        hk416_recipe.json

    # 2. Review hk416_recipe.json, assign any "review" groups, then split.
    cargo run --release -- --split-gun \
        ../noctyrn-game/assets/weapons/models/primary/assault/hk416.glb \
        hk416_recipe.json \
        ./hk416_out

**Step 2 refuses to run while any group is `"review"`** — assign it to a
part (or `"drop"`) in the recipe and re-run. Splitting is deterministic:
the same recipe always produces the same outputs.

**Output layout** (`<out-dir>/`):

    receivers/{gun}.glb                 # core body (everything not detachable)
    attachments/{slot}/{gun}_{part}.glb # barrel, muzzle, optic, stock, ...
    {gun}_sockets.json                  # draft `sockets` block for the gun JSON
    attachment_json/{slot}/{gun}_{part}.json  # draft attachment JSON stubs

**Auto-classification rules** (first match wins, case-insensitive):

| Group name contains... | Part |
|---|---|
| bare number (`55645_14`) or caliber-like (`5.56x45`, `.50 bmg`, `12/70`, `9x19`), bullet/cartridge/casing/shell/round/pellet, `<caliber> case` | `drop` |
| `mag release`/`mag catch`/`mag lock`, ejector/extractor/deflector words | `receiver` (guarded before the ammo rule) |
| `mag`, `clip`, `belt`, `rnd` (but `ammo box` drops first) | `magazine` |
| `barrel` | `barrel` (before muzzle devices: `threaded barrel` stays whole) |
| `muzzle`, `flash hider/guard`, `compensator`, `suppressor`, `silencer`, `brake`, `thread` | `muzzle` |
| `sight`, `scope`, `optic`, `dot`, `holo`, `pso`, `bushnell`, `carry handle`, ... | `optic` |
| `stoc`k/`stoc` typo, `buttpad`, `butt plate`, `butt` (not `button`), `stoock`, `cheek`, `buffer tube` | `stock` |
| `foregrip`, `bipod`, `launcher`, `vertical` | `underbarrel` |
| `handguard`, `grip`, `rail` | `receiver` |
| bolt/carrier/charging/`charge handle`/`trig`ger/selector/`selecotr`/safety/hammer/pin/spring/slide/frame/dust/cover/latch/button/lever/screw/plate/stop/release/catch/`reload handle`/`follower`/`gas tube`/pump/pomp/forend, `mode`/`safe` tokens | `receiver` |
| `light(s)`, `laser`, `flashlight`, `torch`, `lamp`, `peq`, `dbal` | `side` (rail devices; the legacy `other` socket still reads as `side`) |
| `receiver`, `chassis`, `body` | `receiver` |
| anything else | `review` (you decide) |

**Ammo-material rule:** meshes using a material in the recipe's
`ammo_materials` list (default `brass`, `copper`) are dropped — **unless**
their group is a magazine, so loaded mags keep their visible rounds.
Textured primitives (scope reticles) warn and drop individually instead of
failing the gun.

**Rebase:** every part is translated by its plug-in socket position so the
mount point lands at the part file's origin. The runtime then only positions
the entity at the *target* gun's socket — no double offset, and parts stay
portable across guns. Receivers are the frame reference and are never
rebased.

**Names + artist:** display names derive from group names (`flash guard` →
"Flash Guard", `30rnd stanag` → "30-Round STANAG Magazine", wood furniture
→ "Wooden ...") with `"artist": "D_U"` stamped on every stub.

**Socket proposals** (all in the gun model's own frame, identity rotation):

- `barrel`: receiver forward-face center (forward = receiver→barrel direction
  along the dominant horizontal axis)
- `muzzle`: provided by the barrel (its forward-face center). Guns *without*
  a separate barrel part (e.g. AK) get a receiver-provided `muzzle` socket
  derived from the muzzle part's own rear face instead.
- `optic`: receiver top-face center · `magazine`: mag top-face center ·
  `stock`: receiver rear-face center · `underbarrel`: receiver bottom,
  forward third (verify against the handguard) · `side`: receiver side-face
  center (verify the rail side)

Proposals land within centimeters; **verify each in-game** (the gray
placeholder blocks show exactly where sockets landed) and tune the numbers
in the gun JSON. `heuristic` on every proposal says how it was derived.

### Splitter limitations

- Flat PBR materials only — textured *primitives* warn and drop (scope
  reticles); a part that loses *all* geometry aborts loudly. Skins, morph
  targets and non-triangle primitives abort with an error (game GLBs are
  untextured; third-party textured attachments load fine at *runtime*,
  they just can't go *through the splitter*).
- Node transforms are baked into vertices, so parts share the source frame —
  sockets stay valid with no reorientation step.
- Socket *rotations* are always identity; aim/tilt fine-tune lives in the
  attachment JSON's `offset`/`rotation`.

### Part surgery, dedupe and diet

Source models lie: show-off spares hide in receivers, twins tie on height,
and D_U reuses sculpts across guns. Diagnose and fix without re-splitting:

- `--inspect <glb>` — per-group bounds/sizes. The spare is the twin whose
  top-center sits away from the well (compare against the mag catch/trigger
  and the gun JSON socket).
- `--hash-parts <glb>...` — per-mesh shape hashes (world-baked,
  position-invariant, 1mm quantized). **Same hash = same sculpt, merge.**
  Different hash = different sculpt, keep — even when names/sizes match
  (WAC-47 banana body vs STANAG; FN ARKA vs SAKO irons at equal tri counts).
- `--prune-mag <mag> <out> <x,y,z> [keep-substr]` — keep one twin,
  mount-at-origin, prints the new socket. Pass `keep-substr` when review
  overturns the heuristic (RPD-44: the empty twin sits at the well).
  Mount-at-origin means seating = socket: relocating a kept twin to the
  well is just a socket edit, no vertex work.
- `--drop-nodes` / `--extract-nodes` — cut receiver-hiding spares
  (Barrett/Zbroyar) or promote the detailed twin to the mounted part.
- `--translate`, `--append-nodes` — relocate groups, graft lost parts back
  (RPD mag-lock lever).
- `--tri-report` — hottest part files first (iron-sight diet survey).
- `--strip-small <glb> <out> <min_tris>` — drop sub-budget meshes
  (screws/springs in multi-mesh parts). Dense single-mesh irons go back to
  the artist; only mounted parts render, so catalog totals flatter.
- `gltfpack` is **not** part of the workflow: it merges nodes and renames
  them, breaking every name-based command above.

Twin lessons (check these on every new gun): the seated mag sits at the
mag catch with its top flush at the well; the show-off spare lies forward
on the ground, often loaded (brass/copper mats) and lower. Ties go to
loaded — override with `keep-substr` when the empty twin is the seated one.
A fully-buried twin can never be seated (it would be invisible); position,
not load state, decides.
