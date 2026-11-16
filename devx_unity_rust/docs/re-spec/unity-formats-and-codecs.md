# Unity Formats & Codec Algorithms (RE Spec)

> Clean-room specification extracted from `/tmp/devx_decompiled/as/` (`class_918.cs`, `class_922.cs`, `class_833.cs`, `class_897.cs`, `class_896.cs`, `class_901.cs`, `class_912.cs`, `class_915.cs`, `class_914.cs`, `Shader.cs`, `sclass_006.cs`, `Smolv/SmolvDecoder.cs`).

## 1. Container & Stream Formats

1. **AssetBundles (`@as.class_922`, `@as.class_918`)**:
   - Signatures checked in `class_918.smethod_02` / `class_922`: `"UnityFS"`, `"UnityRaw"`, `"UnityWeb"`, `"\xFA..."` (`sfield_02`).
   - Compression modes (`CompressionType` in `class_922.cs:8-16`): `NONE`, `LZMA`, `LZ4`, `LZ4HC`, `LZHAM`, `Lz4Mr0k` (`flags & 0x3F`).
   - External `.resS` / `.resource` stream lookup resolved via `field_12` path + `(field_13 offset, field_14 size)` (`class_915.cs:58-62`, `class_901.cs:730-765`).
2. **WebGL Containers (`@as.class_833`)**:
   - `UnityWeb Compressed Content (gzip)` (`1f 8b` header)
   - `UnityWeb Compressed Content (brotli)`
   - `UnityWebData1.0` packed virtual directory (`begin`, `len`, `name` entries).
3. **Serialized IL2CPP/Type Helper (`dvxil2cReader`)**:
   - Observed format: stream wrapper (`class_140`) -> `GZipStream` -> `"DVXSTI"` magic -> `int version` -> `string unityVersion` -> 32/64-bit metadata tables. (Replaced in Rust via `rabex` embedded AssetRipper TPK database + standard TypeTree).

## 2. Texture2D / Sprite / Cubemap (`@as.class_897`, `@as.class_896`)

- **`TextureFormatEnum` (`class_897.cs:29-100`)**:
  - Uncompressed: `Alpha8 (1)`, `ARGB4444 (2)`, `RGB24 (3)`, `RGBA32 (4)`, `ARGB32 (5)`, `RGB565 (7)`, `R16 (9)`, `RGBA4444 (13)`, `BGRA32 (14)`, `RG16 (62)`, `R8 (63)`.
  - HDR / Float / YUV: `RHalf (15)`, `RGHalf (16)`, `RGBAHalf (17)`, `RFloat (18)`, `RGFloat (19)`, `RGBAFloat (20)`, `YUY2 (21)`, `RGB9e5Float (22)`.
  - Desktop BCn: `DXT1 (10)`, `DXT3 (11)`, `DXT5 (12)`, `BC6H (24)`, `BC7 (25)`, `BC4 (26)`, `BC5 (27)` (+ `texgenpack_texturetype`: `RGTC1`, `RGTC2`, `BPTC_FLOAT`, `BPTC`).
  - Crunch: `DXT1Crunched (28)`, `DXT5Crunched (29)`, `ETC_RGB4Crunched (64)`, `ETC2_RGBA8Crunched (65)`.
  - Mobile: `PVRTC_RGB2 (30)`, `PVRTC_RGBA2 (31)`, `PVRTC_RGB4 (32)`, `PVRTC_RGBA4 (33)`, `ETC_RGB4 (34)`, `ATC_RGB4 (35)`, `ATC_RGBA8 (36)`, `EAC_R (41)`, `EAC_R_SIGNED (42)`, `EAC_RG (43)`, `EAC_RG_SIGNED (44)`, `ETC2_RGB (45)`, `ETC2_RGBA1 (46)`, `ETC2_RGBA8 (47)`, `ASTC_RGB/RGBA_{4x4,5x5,6x6,8x8,10x10,12x12} (48..59)`, `ETC_RGB4_3DS (60)`, `ETC_RGBA8_3DS (61)`.
- **Note**: Unity textures store scanlines bottom-up; PNG/preview pipelines flip vertically to top-down RGBA8.
- **Sprite (`class_896.cs`)**:
  - Reads `m_Rect`, `m_RD.textureRect`, `m_RD.textureRectOffset`, `m_RD.atlasRectOffset`, `m_Pivot`, `m_Border`, `m_PixelsToUnits`, `m_RD.settingsRaw` bitfield (packing mode, rotation, mesh type), and resolves texture either from `m_RD.texture` or `m_SpriteAtlas` via `m_RenderDataKey.first`.

## 3. AudioClip (`@as.class_901`, `sclass_006`)

- **Metadata**: `AudioType`, `AudioCompressionFormat` (`PCM`, `Vorbis`, `ADPCM`, `MP3`, `VAG`, `HEVAG`, `XMA`, `AAC`, `GCADPCM`, `ATRAC9`), legacy `MType` (`AIFF=2`, `MP3=13`, `Ogg_Vorbis=14`, `WAV=20`).
- **Stream Loading**: Supports both inline `m_AudioData` and external `.resS` via `m_Resource.m_Offset` / `m_Resource.m_Size`.
- **FSB5 Container (`sclass_006.cs`)**: Supports FMOD sample modes (`PCM8`, `PCM16`, `PCM24`, `PCM32`, `PCMFLOAT`, `GCADPCM`, `IMAADPCM`, `VAG`, `HEVAG`, `XMA`, `MPEG`, `CELT`, `AT9`, `XWMA`, `VORBIS`), extracting streams to `.ogg`/`.mp3`/`.wav` with standard `"RIFF"` PCM WAV encoding (`class_901.cs:338`).

## 4. Mesh (`@as.class_912`, `@as.class_903`)

- **Streams & Channels**: Reads `m_IndexBuffer` (`IndexFormat`: `UInt16`, `UInt32`), `m_VertexData.m_VertexCount`, `m_VertexData.m_Channels` (`stream`, `offset`, `format`, `dimension`), `m_VertexData.m_DataSize` (or streamed `.resS`), `m_SubMeshes` (`firstByte`, `indexCount`, `topology`, `baseVertex`, `firstVertex`, `vertexCount`, `m_Center`, `m_Extent`).
- **Compressed Meshes**: Supports `m_CompressedMesh` (`m_Normals` + `m_NormalSigns`, `m_Tangents` + `m_TangentSigns`, 5-bit `m_Weights` packed `/ 31f` + `m_BoneIndices`, `m_BindPoses` 4x4 matrices).
- **Exports**: Wavefront OBJ (`MakeMeshAsObj`), ASCII/binary STL (`MakeSTL`), FBX (`IMakeFBX`), glTF/GLB (added in Rust edition), and Unity `.asset`/scene YAML snippets (`AppentToUnityScene`).

## 5. Shaders, TextAssets & Fonts

- **Shader (`@as.Shader`, `SmolvDecoder`, `SpirV`)**:
  - Parses GPU platforms (`GPUPlatform`, `ShaderCompilerPlatform`, `ShaderGpuProgramType`), subprograms, keywords, and material properties.
  - Handles SMOL-V compressed SPIR-V (`SpirVHeaderMagic = 119734787`, `SmolHeaderMagic = 1397575500` in `SmolvDecoder.cs:9-11`), GLSL/GLES cleanup/lifting (`TryDecompile` in `Shader.cs:631-729`), and DXBC disassembly.
- **TextAsset (`@as.class_915`)**:
  - Classifies payload by magic bytes (`class_915.cs:72-95`): `.txt`, UTF-16 Unicode, `.xml` (`<` or UTF-8 BOM + `<`), `.dll` (`MZ`/`PE`), compressed 7z, or `.bytes` binary.
- **Font (`@as.class_914`)**:
  - Inspects `m_FontData` magic bytes (`OTTO` -> `.otf`, otherwise `.ttf`, `class_914.cs:55-60`).
