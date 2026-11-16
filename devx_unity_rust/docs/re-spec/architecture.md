# DevXUnity 11.10 Architecture (RE Spec)

> Clean-room behavioral specification derived from `/home/andlua/Project/DevX/DevXUnity_11.10_Decrypted` using `ilspycmd`, REA (`rea-agents 6.7.0`), `llvm-readobj`, and IDA Pro 9.5 (`idalib`). Licensing, online activation, and DRM/anti-tamper mechanisms are excluded.

## 1. Binary Inventory & Module Boundaries

| Component | Role | REA / Inspection Evidence |
|---|---|---|
| `DevXUnityUnpackerTools.exe` | WinForms + hosted WPF 3D main application (`MainForm`), asset parsers (`@as`), exporters, shader/script inspectors | `ev_5334cdef...`, `ev_e2e32be8...` (`20` module refs, `441` P/Invoke imports), decompiled into `/tmp/devx_decompiled/` |
| `DevXUnityUnpackerUnityCommon.dll` | Shared system paths (`DevXSystemInfo`), extension/common helpers, package info (`PackageInfo`) | `ev_0c1f506b...` (complete coverage) |
| `DevX.Cecil.dll` | Managed IL inspector/writer fork used for `.NET` assembly exploration (`class_880`) | Referenced by `DevXUnityUnpackerTools.exe` (`MainForm.cs:19`) |
| `HelixToolkit*.dll` | WPF 3D viewport (`_3DView.xaml.cs` hosted via `_3DView_WinformsHost.cs`) for OBJ/STL/mesh preview | Referenced by `_3DView_WinformsHost.cs` |
| `Library/TextureConverterWrapper_x64.dll` (+ identical `PicWrapper_x64.dll`) | 1-function thunk `Ponvert` (`0x180001000`) forwarding to `TextureConverter.dll!Qonvert` | Verified via `idalib` + `llvm-readobj` |
| `Library/TextureConverter.dll` | Qualcomm Adreno texture converter (`Qonvert`, `NewTQonvertImage`, etc.) used for ETC/EAC/ATC/DXT/FP conversions (`@as.class_897.method_20`) | Verified via `idalib` |
| `Library/crunch_x64.dll` | Unity Crunch decoder (`DecompressCRN` @ `0x1800047a0`, 106 functions) | Verified via `idalib` & `@as.class_897.smethod_09` |
| `Library/texgenpack_x64.dll` | BC4/BC5/BC6H/BC7 decoder (`texgenpackdecode` @ `0x180003ee0`, 80 functions) | Verified via `idalib` & `@as.class_897.smethod_08` |
| `Library/imagecompress-x64.dll` | Texture encoders (`EXPORT_EncodeToDXT1/DXT5`, `EXPORT_Encode_RGBA_To_ETC2_*`), `appDecompressLZX` (`0x180003980`), `crn_convert_file_to_png` (`0x1800040f0`) | Verified via `idalib` & `class_155.cs` |
| `Library/fmod_x64.dll` | FMOD runtime (`FMOD.class_253`) + external `FSBTool` CLI for decoding Unity `FSB5` `AudioClip` assets (`@as.class_901`) | Verified via `@as.class_901.cs` & `FMOD/class_253.cs` |
| `Library/D3Dcompiler_47.dll` | `D3DDisassemble` P/Invoke (`sclass_002.cs:7`) for DXBC shader disassembly | Verified via `ev_e2e32be8...` & `sclass_002.cs` |
| `StreamingAssets/ArmCP/x64/arm_cp.dll` | Standard Capstone disassembly engine (`cs_open`, `cs_disasm`, etc., `DSMCaps.CapstoneDisassembler`) | Verified via `idalib` (1388 funcs, 22 Capstone exports) |

## 2. Core Data Pipeline & Class Architecture

1. **Project & VFS Manager (`class_111.cs`)**:
   - Coordinates multi-container ingestion: Game Directories, `.assets` / split `.splitN`, `UnityFS` / `UnityRaw` / `UnityWeb` bundles (`@as.class_918`, `@as.class_922`), WebGL `.unityweb` / `UnityWebData1.0` (`@as.class_833`), APKs/IPAs/OBBs/UnityPackages.
   - Maintains indexed asset catalog (`SortedDictionary<long, class_944>`) and background UI progress dispatching (`MainForm.class_004`).
2. **Typed Asset Factory (`@as.class_713.cs:220` & `@as.class_944.cs`)**:
   - Base asset node `class_713` implements `IContent`, `IContentExternID`, `IContentBinary`, `IContentResBinary`, `IContentResBinaryReplace`.
   - `ClassIDEnum` maps Unity class IDs to specialized handlers:
     - Scene graph: `GameObject` (`class_913`), `Transform` (`class_906`), `RectTransform` (`class_874`)
     - Rendering: `Texture2D` (`class_897`), `Cubemap` (`class_898`), `Texture3D` (`class_733`), `Sprite` (`class_896`), `Material` (`class_905`), `Shader` (`Shader`), `Mesh` (`class_912`), `MeshFilter` (`class_904`), `MeshRenderer` (`class_903`), `SkinnedMeshRenderer` (`class_860`)
     - Audio & Fonts: `AudioClip` (`class_901`), `AudioSource` (`class_889`), `Font` (`class_914`), `TextMesh` (`class_887`)
     - Scripts & Data: `TextAsset` (`class_915`), `MonoBehaviour` (`class_892`), `MonoScript` (`class_894`), `CGProgram` (`class_732`)
     - Animation & Physics: `Animator` (`class_888`), `BoxCollider` (`class_902`), `MeshCollider` (`class_886`), `SphereCollider` (`class_891`)
3. **Export Interfaces**:
   - `IContentExportable` (`ExportFileName`, `ExportContent` in `IContentExportable.cs`)
   - Mesh exporters (`IMakeObj`, `IMakeSTL`, `IMakeFBX`, `IMakeUnityMesh` implemented by `class_912` and `class_903`)
   - Bulk export modes (`ExportMode.cs`): `AssetItem`, `ExportToDir`, `ExportForRepack`, `UnityProject`, `UnityPackage`, `UnityPerfab`, `UnityScene`, `ProjectScripts`.

## 3. Clean-Room Rust Replacement Mapping

- Replace `.NET Framework` + WinForms/WPF + Windows-only DLLs with cross-platform Rust workspace (`devx_unity_rust`):
  - `rabex 0.2.1` + `unity-asset-binary 0.2.0` + `unity-asset-decode 0.2.0` for container/SerializedFile/TypeTree R/W.
  - `texture2ddecoder 0.1.2` + `image 0.25.10` replacing `TextureConverter.dll`, `crunch_x64.dll`, `texgenpack_x64.dll`.
  - `fsbex 0.3.0` + `symphonia 0.6.1` + `hound 3.5.1` + `rodio 0.23.0` replacing `fmod_x64.dll` and `FSBTool`.
  - Pure-Rust SMOL-V/SPIR-V/DXBC shader inspection + `capstone` replacing `d3dcompiler_47.dll` and `arm_cp.dll`.
  - `eframe` / `egui 0.37.0` + `egui_dock 0.21.1` replacing `MainForm` + `HelixToolkit`.
