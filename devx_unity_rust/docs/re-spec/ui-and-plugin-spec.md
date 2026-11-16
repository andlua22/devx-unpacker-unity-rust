# UI Workbench & Plugin System (RE Spec)

> Clean-room UI and extensibility specification derived from `MainForm.cs`, `DevXCControl.cs`, `DevXC.cs`, and `StreamingAssets/DevXC/*.devxc`.

## 1. Workbench Layout & Navigation (`MainForm.cs`)

1. **Multi-Mode Container Opening**:
   - Open Unity Game Directory (scanning `*_Data/`, `level*`, `sharedassets*.assets`, `resources.assets`, `StreamingAssets/`)
   - Open single `.assets` / split `.split0..N`
   - Open AssetBundle (`.unity3d`, `.bundle`, no-extension UnityFS)
   - Open Mobile Packages (`.apk`, `.xapk`, `.obb`, `.ipa`)
   - Open WebGL Builds (`.unityweb`, `UnityWebData1.0` + `.data.br`/`.data.gz`)
   - Open `.unitypackage` archives (`tar.gz` with `<guid>/asset`, `asset.meta`, `pathname`)
2. **Asset Tree & Filtering**:
   - Multi-select tree (`MultiSelectTreeView2`) with search filter & class filter.
   - View modes:
     - **By Asset Type** (`Texture2D`, `Sprite`, `AudioClip`, `Mesh`, `TextAsset`, `Shader`, `Material`, `MonoBehaviour`, `Font`, `GameObject`, etc.)
     - **By Container / Bundle / SerializedFile**
     - **By Scene / GameObject Hierarchy** (`GameObject` -> `Transform` children + attached components)
     - **Flat / Filtered Search**
3. **Preview & Inspection Tabs**:
   - **Image / Sprite**: checkerboard/dark/light background switcher, zoom/pan, RGBA channel isolator (R/G/B/A), mipmap/format/dimensions overlay, Sprite slice overlay (`m_Rect`, `m_Border`, `m_Pivot`).
   - **AudioClip**: waveform visualizer, play/pause/stop + scrub bar (`rodio`), sample rate/channels/compression/duration readout, export WAV/OGG/MP3/raw.
   - **Mesh**: interactive 3D orbit/pan/zoom preview (`wgpu` or projected wireframe/shaded viewport), submesh selector, vertex/triangle/channel stats, export `.obj` / `.stl` / `.glb` (glTF).
   - **TextAsset / Shader / MonoScript**: syntax-highlighted viewer (`syntect` via `egui_extras`), hex fallback when binary, copy/export.
   - **TypeTree & YAML/JSON Inspector**: collapsible property tree + raw YAML/JSON text view; **clickable `PPtr(fileID, pathID)` links** jumping directly to referenced assets + **Used By** reverse reference list.
   - **Virtual Hex / Binary RAW Viewer**: offset + 16-byte hex + ASCII columns with search & raw dump.
4. **Background Jobs & Status Bar**:
   - Non-blocking background worker queue (`MainForm.class_004` equivalent) with cancelable progress and status/console log dock.

## 2. Plugin Architecture (`DevXC.cs` + `DevXCControl.cs` -> `devx-plugin-api`)

DevX exposed scripted automation and post-processing hooks in `StreamingAssets/DevXC/*.devxc` (`Startup`, `ConvertTexture`, `AutioConvertor`, `ShaderPostProcessor`, `MaterialPostProcessor`, `AssemblyPostProcessor`, `SourcesPostProcessor`) alongside the host API listed in `DevXCControl.cs:149`:

| DevX Host API Group | Recovered Operations (`DevXCControl.cs:149`) | Rust `devx-plugin-api` Mapping |
|---|---|---|
| Diagnostics & Status | `warning`, `error`, `ShowStatus`, `ShowDebug`, `ClearDebug` | `PluginContext::log_warn / log_error / set_status` |
| Asset Query & Enumeration | `FindPPtr`, `FindAssets`, `EnumByAssetType`, `EnumAllAssets`, `EnumAllAssetsFiltered` | `AssetCatalogView::find_pptr`, `find_assets`, `enum_by_class`, `iter_all`, `iter_filtered` |
| Object & Field Access | `MakeAssetObject`, `GetAssetField`, `SetAssetField`, `GetAssetFieldsYamp`, `GetName`, `GetAssetType`, `UnityVersion` | `AssetHandle`, `TypeTreeValue` serde reflection + `get_field` / `set_field` |
| Cross-References & GUIDs | `ChangeToPPtr`, `GetAssetGuid`, `GetAssetID`, `GetMonoScriptGuid`, `GetMonoScriptID` | `PPtr`, `asset_guid`, `mono_script_guid`, reverse ref queries |
| Domain Export & Replace | `SavePNG`, `SaveOBJ`, `MakeFBX`, `SaveAsWAV`, `SaveSubContent`, `SaveDUMP`, `ExportToDir`, `MakePrefabs`, `MakeUnityUnitypackage`, `ReplaceImage`, `ReplaceText`, `ReplaceAudio` | `AssetDecoderPlugin`, `AssetExporterPlugin`, `PostProcessorPlugin` traits + `HostActions` |
| Compression / Binary Stream | `GzipCompress/Decompress`, `BrotliDecompress`, `LZ4Decompress`, `LZMADecompress`, endian/aligned binary reader | Exposed via `devx-unity-io` stream & compression utilities |

Our Milestone 1 Rust implementation delivers all these host capabilities via typed, safe Rust traits (`DevXPlugin`, `StartupHook`, `TexturePostProcessor`, `AudioPostProcessor`, `ShaderPostProcessor`, `MaterialPostProcessor`, `AssetExporterPlugin`) with optional scripted plugins cleanly isolated from DRM/network execution.
