# DevX Unpacker Unity Rust

Clean-room, cross-platform Unity asset inspector, decoder, IL2CPP/Mono analyzer, interactive 2D/3D viewer, and multi-format exporter built in Rust.

> Reimplements standard Unity container/asset parsing, codecs, UI layout previewing, script metadata/IL2CPP analysis, and export pipelines in Rust without proprietary licensing checks or DRM bypasses.

## Repository Structure

```text
.
├── .gitignore
├── README.md
└── devx_unity_rust/
    ├── Cargo.toml                 # Cargo workspace (Rust 2024 edition)
    ├── LICENSE                    # GNU Affero General Public License v3.0
    ├── crates/
    │   ├── devx-app/              # Desktop egui/eframe workbench GUI
    │   ├── devx-core/             # Asset catalog, PPtr reference graph, search & job runner
    │   ├── devx-unity-io/         # UnityFS/Raw/Web bundles, SerializedFile, APK/ZIP VFS & TPK bridge
    │   ├── devx-codecs/           # Texture2D, Mesh/FBX/glTF/DAE, Audio (FSB5), Shader (SPIR-V/DXBC), Animation
    │   ├── devx-scripts/          # Mono PE/CLI & IL2CPP metadata inspector + disassembly integration
    │   ├── devx-exporter/         # Batch unpack, Prefab/Scene/UnityProject, UnityFS repack, Bevy 0.20 export & UI Canvas
    │   ├── devx-plugin-api/       # Plugin trait & host context abstractions
    │   └── devx-plugins-builtin/  # Built-in exporter & texture post-processor plugins
    ├── docs/re-spec/              # Clean-room behavioral specifications & evidence JSONs
    └── vendor/il2cpp_dumper/      # Vendored il2cpp-dumper-rs core (ARM/x86 disasm & C#/pseudo-C recovery)
```

## Key Features

- **Containers & VFS (`devx-unity-io`)**: Reads `UnityFS` / `UnityRaw` / `UnityWeb` bundles, `.assets` / `.splitN`, `.unitypackage`, and Android `.apk` / `.xapk` / `.obb` / `.zip` archives with lazy/deferred bundle loading and automatic dependency resolution (`rabex` + `unity-asset-binary`).
- **Desktop Workbench (`devx-app`)**:
  - Multi-mode asset tree, `SerializedFile` TypeTree inspector, `PPtr` reference graph, console & search.
  - 3D mesh/rigged scene viewport, texture & sprite preview, shader viewer, and audio playback (`rodio`).
  - **Interactive 2D uGUI Canvas Preview**: Renders `Canvas`, `CanvasGroup`, `CanvasRenderer`, `RectTransform` top-down layout, `CanvasScaler`, `Mask`/`RectMask2D`, `Image` (including 9-slice & fill/aspect), `RawImage`, and `Text`/`TMP_Text` with resolution presets, zoom/pan, node picking, and layout JSON export.
  - Script & IL2CPP browser with disassembly and C#/pseudo-C decompilation view.
- **Pure-Rust Codecs (`devx-codecs`)**:
  - Textures: `ASTC`, `ETC`, `PVRTC`, `BCn`/`DXT`, `Crunch` -> `PNG`, `JPEG`, `WebP`, `BMP`, `TGA`, `DDS`.
  - 3D & Animation: `GLB`, `glTF`, `FBX`, `DAE`, `OBJ`, `PLY`, `STL` including skeletons, skinning, blend shapes, and `Animator` simulation.
  - Audio: `FSB5` / `WAV` via `fsbex`, `symphonia`, `hound`.
  - Shaders: `ShaderLab`, `CG`, `SMOL-V` / `SPIR-V`, and `DXBC` inspection/disassembly.
- **Scripts & IL2CPP (`devx-scripts` + `vendor/il2cpp_dumper`)**:
  - Inspects `Managed/*.dll` (Mono PE/CLI metadata) and `global-metadata.dat` + `libil2cpp.so`.
  - Supports ARM64/x86/x86_64 disassembly (`yaxpeax-arm`, `iced-x86`), CFG analysis, and method body recovery.
- **Exporters & Playable Bevy 0.20 Sandbox (`devx-exporter`, `devx_cli`)**:
  - Unpack individual assets, export hierarchies/prefabs/scenes/full Unity projects, or repack bundles.
  - Generate standalone runnable **Bevy `0.20.1`** projects (`export-bevy`) featuring `SceneExplorer` (`F1`), `BattleStage` (`F2`), `ModelStudio` (`F3`), and interactive `UiPanels` (`F4` + `U`/`I` switching, 9-slice rendering, clickable `Button` widgets).

## Build & Run

Run Cargo commands inside `devx_unity_rust/`:

```bash
cd devx_unity_rust

# Build workspace binaries (GUI + CLI)
cargo build --release --bin devx-app --bin devx_cli

# Launch the desktop workbench
cargo run --release --bin devx-app

# Run linter and test suite
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## CLI Usage (`devx_cli`)

```text
devx_cli inspect <input> [--filter <substr>] [--dump <name_or_pathid>] [--with-deps] [--all-bundles]
devx_cli extract-ab <input> <out_dir> [--filter <substr>]
devx_cli unpack <input> <out_dir> [--class <ClassName>] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--tex png|jpg|webp|bmp|tga|dds] [--mesh obj|fbx|glb|dae|ply|stl] [--limit <N>]
devx_cli decompile-shaders <input> <out_dir> [--bundle-filter <substr>] [--all-bundles]
devx_cli export-glb <input> <out.glb> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
devx_cli export-fbx <input> <out.fbx> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
devx_cli export-gltf <input> <out.gltf> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
devx_cli export-dae <input> <out.dae> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
devx_cli export-hierarchy <input> <out_dir> [--format glb|gltf|fbx|dae|obj] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--limit <N>] [--max-tex <px>]
devx_cli export-prefab <input> <out_dir_or_pkg> [--name <stem>] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--unitypackage] [--no-companions]
devx_cli export-scene <input> <out_dir_or_pkg> [--name <stem>] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--unitypackage] [--no-companions]
devx_cli export-bevy <input> <out_dir> [--name <pkg>] [--scene <substr>]... [--char <substr>]... [--prop <substr>]... [--anim <substr>]... [--ui-panel <substr>]... [--max-scenes <N>] [--max-chars <N>] [--max-props <N>] [--max-ui-panels <N>] [--max-tex <px>]
devx_cli export-unity-project <input> <out_dir> [--bundle-filter <substr>] [--all-bundles] [--max-scenes <N>] [--max-prefabs <N>] [--no-scripts]
devx_cli dump-il2cpp <input> <out_dir> [--disasm] [--pseudo-c] [--fold-ifix] [--body-mode <off|assembly|csharp|both>] [--dummy-dll] [--max-insns <N>] [--no-hex] [--no-cfg] [--no-annotations] [--no-field-names] [--filter-asm <name>]... [--filter-type <name>]...
devx_cli disasm-il2cpp <input> (--rva <0xHEX> | --type <TypeName>) [--method <MethodName>] [--asm <Assembly>] [--all] [--limit <N>] [--max-insns <N>] [--no-hex] [--no-cfg] [--no-annotations] [--no-field-names]
```

### Example: Generate & Run a Bevy 0.20 Showcase Project

```bash
cd devx_unity_rust
cargo run --release --bin devx_cli -- export-bevy /path/to/game.apk ./exported_bevy \
  --ui-panel loadingpanel \
  --max-scenes 2 --max-chars 2 --max-props 2 --max-ui-panels 4

cd ./exported_bevy
cargo run --release
```

## License

- Repository license: [devx_unity_rust/LICENSE](devx_unity_rust/LICENSE) (GNU Affero General Public License v3.0 — AGPL-3.0; workspace crate manifests in [devx_unity_rust/Cargo.toml](devx_unity_rust/Cargo.toml) also list `MIT OR Apache-2.0`).
- Vendored [il2cpp_dumper](devx_unity_rust/vendor/il2cpp_dumper/Cargo.toml) (`il2cpp-dumper-rs` core) is licensed under **`GPL-3.0-or-later`** ([devx_unity_rust/vendor/il2cpp_dumper/LICENSE](devx_unity_rust/vendor/il2cpp_dumper/LICENSE)).
