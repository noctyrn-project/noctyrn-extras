# noctyrn-extras

Content pipeline tools for the Noctyrn game.

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
