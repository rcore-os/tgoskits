# cpu-model-test - the "pyte for 3D models"

An industrial-grade 3D-model test carpet for StarryOS covering **mesh loading + 3D-print slicing +
software rasterization + point clouds**. Where `pyte` gives a headless terminal you can assert against
cell-by-cell, this gives a headless mesh/geometry pipeline you can assert against **with closed-form or
byte-exact goldens**: every cell drives a real parser / slicer / rasterizer and checks the output against a
closed-form property (a unit-cube slice is a square of perimeter 4.0 and area 1.0; a sphere-sampled cloud
has its centroid at the origin and every point at distance r; a front-facing cube projects to a solid
centered square whose depth is the front face) or a value calibrated once host-side with this exact code
(bunny.ply vertex count / bbox / centroid / spatial-hash signature; suzanne render coverage + depth
signature; per-layer slice perimeter/area/segment-count). "Model loaded" alone is not a test here.

## What is self-written vs reused

The OBJ / STL (binary + ASCII) / PLY (ASCII + binary) parsers, the mesh-plane slicer, the barycentric +
z-buffer software rasterizer, the point-cloud stats and all the comparison logic (SHA-256, spatial-hash,
closed-form checks) are **self-written** under `programs/carpets/`. These formats are small and well
specified, so a clean parser is not "reinventing a heavy lib" - and writing them ourselves is exactly what
lets the cube parse identically across five independent readers.

glTF/glb is a JSON+binary container, so it is **not** hand-rolled: the glb leg uses the vendored
single-header **cgltf.h** (v1.15, `jkuhlmann/cgltf`, fetched from official GitHub raw). No heavy mesh
library (assimp) is pulled; the cells link statically, so on target there is **no runtime 3D-library
dependency** - only libc + libm.

```
cgltf: https://raw.githubusercontent.com/jkuhlmann/cgltf/de9828bc6419064c302546313ce8ff5eac6cd703/cgltf.h  (v1.15)
```

## Build and run

`prebuild.sh` cross-compiles the five cells fully static against musl (libc + libm only) on the host with a
musl-cross toolchain resolved from `<triple>-gcc` on PATH, then `/opt/<triple>-cross`, then `zig cc`, then
`musl-gcc` for a native build - no qemu-user, no prebuilt binaries. `third_party/cgltf.h` is excluded by the
repo `.gitignore`, so `prebuild.sh` fetches it from the pinned upstream commit
`jkuhlmann/cgltf@de9828bc6419064c302546313ce8ff5eac6cd703` and verifies its SHA-256 before compiling; a fetch
failure or hash mismatch aborts the build. Run on each architecture (single vCPU, `-smp 1`):

```
cargo xtask starry app qemu -t cpu-model-test --arch x86_64
cargo xtask starry app qemu -t cpu-model-test --arch aarch64
cargo xtask starry app qemu -t cpu-model-test --arch riscv64
cargo xtask starry app qemu -t cpu-model-test --arch loongarch64
```

The derived closed-form assets (cube in 5 formats + sphere cloud) are always generated host-side by
`tools/gen_goldens.py`, so the closed-form legs run without any external data. The real models
(suzanne/benchy/bunny) are provisioned from the per-app `assets` git submodule; `prebuild.sh` inits and
LFS-pulls it automatically, or run it manually from the app dir first:

```
git submodule update --init apps/starry/cpu-model-test/assets
git -C apps/starry/cpu-model-test/assets lfs pull --include="models/*,pointcloud/*,golden/*"
```

If the submodule is absent, set `MODEL_ASSET_SRC` / `PC_ASSET_SRC` to a `render-assets` tree; the
real-model legs honest-skip when no models are found, and the closed-form legs still gate.

## Assets

Staged into the image under `/opt/cpu-model-test/assets`:

- **Real models** (from `render-assets/models`): `suzanne.obj` (507 v / 500 f, quads),
  `suzanne.stl` (968 tris), `suzanne.glb`, `benchy.stl` (16186 tris, the 3DBenchy print-test model).
- **Real point cloud** (from `render-assets/pointcloud`): `bunny.ply` (Stanford bunny, 35947 verts /
  69451 triangular faces, ASCII PLY with x y z confidence intensity).
- **Derived closed-form assets** (generated host-side by `tools/gen_goldens.py`, always staged): a KNOWN
  unit cube in **five formats** - `cube.obj`, `cube.stl` (binary), `cube_ascii.stl`, `cube.ply` (ASCII),
  `cube_bin.ply` (binary LE) - so OBJ==STL==PLY and ASCII==binary can be cross-asserted; and
  `sphere_pc.ply`, a Fibonacci-sphere-sampled point cloud (4000 points at radius 2.5, centroid at origin).

Asset-dependent legs honest-skip if `MODEL_DIR` / the submodule is absent; the closed-form (in-memory or
derived-cube/sphere) legs always run, so every cell always has assertions.

## Cells

Each cell prints `MODEL_<CELL> OK <n>` only when `fail==0 && total==pass==<n>` (three-gate). `run_all.sh`
gates on the capability manifest: `fail==0 && total==EXPECTED==pass`, EXPECTED>=1 floor. Assertion counts
below are the real host green run with all assets present.

### `model_parse` - mesh format loaders - 38 assertions

- **KNOWN cube across five readers**: `cube.stl` (binary), `cube_ascii.stl`, `cube.obj`, `cube.ply`
  (ASCII), `cube_bin.ply` (binary) all parse to **identical geometry** - exact 12-triangle count, exact
  8-corner unique-vertex set, exact bounding box `[0,1]^3`, and each reader's vertex set contains all 8
  cube corners. Five independent format decoders converging on the same geometry is the strongest possible
  parse assertion. OBJ quad/tri and PLY face lists are fan-triangulated on load.
- **suzanne**: OBJ (507 verts, 500 faces -> 968 triangles after quad fan-triangulation) and STL (968
  triangles) parse; bbox matches the slice golden; OBJ-triangulated tri count == STL tri count.
- **benchy STL**: 16186 triangles, bbox z `[0,48]`.
- **glTF/glb**: `suzanne.glb` parsed with vendored cgltf - valid glTF 2.0, >=1 mesh, >=1 primitive.
- **negative controls** (in-memory, asset-independent): a well-formed 2-triangle binary STL parses, the same
  header truncated is rejected, and a PLY with no `end_header` or an unknown format token is rejected - the
  parsers do not silently accept garbage.

### `model_slice` - 3D-print slicer (the KEY closed-form leg) - 52 assertions

Mesh-plane intersection at height Z -> contour segments -> per-layer perimeter / area / loop-count. The
segment extraction and raw-segment shoelace mirror `render-assets/models/slice_golden.py` exactly.

- **unit cube** sliced at Z in {0.25, 0.5, 0.75}: exactly 8 crossing segments forming **one loop** with
  **perimeter 4.0 and area 1.0 to 1e-9** - the analytic square, asset-independent closed form.
- **tessellated cylinder** (r=1, h=2, N=128 sides) sliced at mid-height: exactly 2N=256 segments forming
  one loop; the contour perimeter and area are strictly less than but converge to the analytic circle
  (`2*pi*r`, `pi*r^2`) within the O(1/N^2) discretization bound (2e-3).
- **suzanne** and **benchy** sliced at the golden Z heights: per-layer perimeter, area and segment count
  match `slice_golden.json` (tolerance 1e-3). This proves the slicer geometry on real meshes.
- **edge/negative controls**: slicing the cube at z=2.0 and the cylinder above its z-range yields an empty
  contour (0 segments / 0 loops / 0 perimeter / 0 area), and discriminators confirm the closed-form gates
  would reject a wrong golden (cube perimeter is not 3.0; the interior cut had nonzero perimeter).

### `model_render` - rasterize a mesh -> pixels - 11 assertions

Barycentric + z-buffer software rasterizer (like the render scene_3dmodel cells).

- **unit cube, front view**: the silhouette is a **solid** filled rectangle (no interior holes),
  **square** (w~h) and **centered** in the frame; the visible face is the **front** face, so every covered
  pixel's depth is uniform (a plane perpendicular to the view axis). Pushing the cube farther yields fewer
  covered pixels (perspective) and **strictly greater depth everywhere** (depth monotonic in distance) -
  closed-form occlusion + projection.
- **two-triangle occlusion scene**: a near triangle in front of a far triangle at the same screen position
  -> every covered pixel shows the **near** triangle's depth, proving nearest-wins z-buffering.
- **suzanne** rendered at a fixed 3/4 view: covered-pixel count (17256) + downscaled depth signature SHA
  vs the calibrated golden.

### `model_pointcloud` - PLY point cloud - 17 assertions

- **synthetic sphere cloud** (`sphere_pc.ply`, 4000 points): exact count, **centroid at the origin**
  (|c| < 1e-3), and **every point at radius r** (rmax-rmin < 1e-4, r == 2.5) - the fully deterministic
  closed-form leg.
- **Stanford bunny** (`bunny.ply`): exact vertex count **35947**, bounding box, centroid, and a **16^3
  spatial-hash occupancy signature** (SHA-256 over the LE uint32 grid counts). A single displaced or
  dropped point flips the signature.
- **mutation control** (in-memory, asset-independent): a fixed synthetic cloud's signature is computed, then
  one point is displaced and one point is dropped, each asserted to change the signature (and the drop to
  lower the count) - the sensitivity claimed above is exercised in code, not just described.

### `model_realassets` - iterate every shipped model - 13 assertions

Walks every real model and asserts it parses through the self-written parsers with a hard golden:
suzanne.obj (507 v / 968 t / width), suzanne.stl (968 t / bbox), benchy.stl (16186 t / bbox), bunny.ply
(35947 v / 69451 t / bbox), suzanne.glb (cgltf, >=1 mesh); >=4 real assets parsed. Honest-skip the whole
cell if `MODEL_DIR` is absent.

## Determinism of the goldens

The slicer's raw-segment shoelace matches `slice_golden.py` exactly, so the per-layer perimeter/area
goldens reproduce across arches. The spatial-hash signature quantizes into a fixed 16^3 integer grid and
hashes the LE uint32 counts, so it is integer-exact given the same parsed doubles (all float parsing goes
through `strtod`). The software rasterizer is a fixed integer-framebuffer pipeline, so its coverage count
and depth signature are reproducible. The cube slice (perimeter 4 / area 1) and the sphere cloud (centroid
0, radius r) are analytic closed forms independent of any calibration. A parse divergence, a slicer bug, a
z-buffer regression or a displaced point flips a closed-form check or a golden and the cell FAILs loudly.

**Mutation-tested**: perturbing the suzanne z=0 slice-area golden (0.569877 -> 0.579877) fails
`model_slice`; changing the expected bunny vertex count (35947 -> 35948) fails `model_pointcloud`;
displacing a single bunny vertex flips the spatial-hash signature and fails `model_pointcloud`.

## Coverage

- **Mesh formats**: OBJ (ASCII, tri + quad), STL (binary + ASCII), PLY (ASCII + binary_little_endian),
  glTF/glb (via cgltf). 4 mainstream formats, 6 encodings.
- **Slicing**: closed-form cube (square) + cylinder (circle), real-mesh per-layer goldens (suzanne, benchy).
- **Rasterization**: silhouette / solidity / centering, depth occlusion (front-face + nearest-wins z-buffer
  + perspective depth monotonicity), real-mesh coverage + depth signature.
- **Point clouds**: closed-form sphere (centroid / radius) + real bunny (count / bbox / centroid / sig).

## Layout

```
cpu-model-test/
  prebuild.sh                       # host cross-compile (musl-cross) + fetch/verify cgltf.h + generate/stage assets into the overlay
  build-<arch>.toml x4              # ArceOS build features per arch
  qemu-<arch>.toml x4               # QEMU boot + run_all.sh + success/fail regex per arch
  tools/
    gen_goldens.py                  # host-side: emit derived cube/sphere assets + recompute bunny golden
  programs/
    run_all.sh                      # on-target three-gate runner
    carpets/
      model_common.h                # SHA-256 + gate + mesh types + bbox (self-written)
      model_parse.h                 # OBJ / STL(bin+ascii) / PLY(ascii+bin) parsers (self-written)
      model_slice.h                 # mesh-plane intersection + shoelace + loop counter (self-written)
      model_raster.h                # mat4 + barycentric + z-buffer rasterizer (self-written)
      model_pointcloud.h            # centroid / radius / spatial-hash signature (self-written)
      model_parse.c                 # cell 1
      model_slice.c                 # cell 2
      model_render.c                # cell 3
      model_pointcloud.c            # cell 4
      model_realassets.c            # cell 5
      third_party/
        cgltf.h                     # pinned single-header glTF 2.0 parser (v1.15, jkuhlmann/cgltf)
```
