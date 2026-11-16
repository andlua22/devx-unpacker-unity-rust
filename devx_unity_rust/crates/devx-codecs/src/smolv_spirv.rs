//! Clean-room SMOL-V compressed SPIR-V decoder and human-readable SPIR-V
//! reflection / instruction disassembler.
//!
//! SMOL-V wire layout follows Aras Pranckevičius's open-source SMOL-V format
//! specification (`0x534D4F4C` header, packed varints, delta/zigzag IDs,
//! compact vector swizzles, and op-word-count bias encoding).

use std::collections::HashMap;
use std::fmt::Write as _;

/// SMOL-V stream magic word (`1397575500u32`, ASCII `"LOMS"` in LE).
pub const SMOL_MAGIC: u32 = 0x534D_4F4C;

/// Standard SPIR-V stream magic word (`0x07230203`).
pub const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Per-opcode structural metadata used by SMOL-V varint/delta decoding.
#[derive(Clone, Copy)]
struct OpMeta {
    has_result: bool,
    has_type: bool,
    delta_from_result: i8,
    var_rest: bool,
}

const fn m(has_result: u8, has_type: u8, delta_from_result: i8, var_rest: u8) -> OpMeta {
    OpMeta {
        has_result: has_result != 0,
        has_type: has_type != 0,
        delta_from_result,
        var_rest: var_rest != 0,
    }
}

/// Metadata table for known SPIR-V opcodes `0..331` (matching SMOL-V spec).
const SPIRV_OP_DATA: [OpMeta; 331] = [
    m(0, 0, 0, 0),  // 0 Nop
    m(1, 1, 0, 0),  // 1 Undef
    m(0, 0, 0, 0),  // 2 SourceContinued
    m(0, 0, 0, 1),  // 3 Source
    m(0, 0, 0, 0),  // 4 SourceExtension
    m(0, 0, 0, 0),  // 5 Name
    m(0, 0, 0, 0),  // 6 MemberName
    m(0, 0, 0, 0),  // 7 String
    m(0, 0, 0, 1),  // 8 Line
    m(1, 1, 0, 0),  // 9
    m(0, 0, 0, 0),  // 10 Extension
    m(1, 0, 0, 0),  // 11 ExtInstImport
    m(1, 1, 0, 1),  // 12 ExtInst
    m(1, 1, 2, 1),  // 13 VectorShuffleCompact (SMOL-V pseudo-op)
    m(0, 0, 0, 1),  // 14 MemoryModel
    m(0, 0, 0, 1),  // 15 EntryPoint
    m(0, 0, 0, 1),  // 16 ExecutionMode
    m(0, 0, 0, 1),  // 17 Capability
    m(1, 1, 0, 0),  // 18
    m(1, 0, 0, 1),  // 19 TypeVoid
    m(1, 0, 0, 1),  // 20 TypeBool
    m(1, 0, 0, 1),  // 21 TypeInt
    m(1, 0, 0, 1),  // 22 TypeFloat
    m(1, 0, 0, 1),  // 23 TypeVector
    m(1, 0, 0, 1),  // 24 TypeMatrix
    m(1, 0, 0, 1),  // 25 TypeImage
    m(1, 0, 0, 1),  // 26 TypeSampler
    m(1, 0, 0, 1),  // 27 TypeSampledImage
    m(1, 0, 0, 1),  // 28 TypeArray
    m(1, 0, 0, 1),  // 29 TypeRuntimeArray
    m(1, 0, 0, 1),  // 30 TypeStruct
    m(1, 0, 0, 1),  // 31 TypeOpaque
    m(1, 0, 0, 1),  // 32 TypePointer
    m(1, 0, 0, 1),  // 33 TypeFunction
    m(1, 0, 0, 1),  // 34 TypeEvent
    m(1, 0, 0, 1),  // 35 TypeDeviceEvent
    m(1, 0, 0, 1),  // 36 TypeReserveId
    m(1, 0, 0, 1),  // 37 TypeQueue
    m(1, 0, 0, 1),  // 38 TypePipe
    m(0, 0, 0, 1),  // 39 TypeForwardPointer
    m(1, 1, 0, 0),  // 40
    m(1, 1, 0, 0),  // 41 ConstantTrue
    m(1, 1, 0, 0),  // 42 ConstantFalse
    m(1, 1, 0, 0),  // 43 Constant
    m(1, 1, 9, 0),  // 44 ConstantComposite
    m(1, 1, 0, 1),  // 45 ConstantSampler
    m(1, 1, 0, 0),  // 46 ConstantNull
    m(1, 1, 0, 0),  // 47
    m(1, 1, 0, 0),  // 48 SpecConstantTrue
    m(1, 1, 0, 0),  // 49 SpecConstantFalse
    m(1, 1, 0, 0),  // 50 SpecConstant
    m(1, 1, 9, 0),  // 51 SpecConstantComposite
    m(1, 1, 0, 0),  // 52 SpecConstantOp
    m(1, 1, 0, 0),  // 53
    m(1, 1, 0, 1),  // 54 Function
    m(1, 1, 0, 0),  // 55 FunctionParameter
    m(0, 0, 0, 0),  // 56 FunctionEnd
    m(1, 1, 9, 0),  // 57 FunctionCall
    m(1, 1, 0, 0),  // 58
    m(1, 1, 0, 1),  // 59 Variable
    m(1, 1, 0, 0),  // 60 ImageTexelPointer
    m(1, 1, 1, 1),  // 61 Load
    m(0, 0, 2, 1),  // 62 Store
    m(0, 0, 0, 0),  // 63 CopyMemory
    m(0, 0, 0, 0),  // 64 CopyMemorySized
    m(1, 1, 0, 1),  // 65 AccessChain
    m(1, 1, 0, 0),  // 66 InBoundsAccessChain
    m(1, 1, 0, 0),  // 67 PtrAccessChain
    m(1, 1, 0, 0),  // 68 ArrayLength
    m(1, 1, 0, 0),  // 69 GenericPtrMemSemantics
    m(1, 1, 0, 0),  // 70 InBoundsPtrAccessChain
    m(0, 0, 0, 1),  // 71 Decorate
    m(0, 0, 0, 1),  // 72 MemberDecorate
    m(1, 0, 0, 0),  // 73 DecorationGroup
    m(0, 0, 0, 0),  // 74 GroupDecorate
    m(0, 0, 0, 0),  // 75 GroupMemberDecorate
    m(1, 1, 0, 0),  // 76
    m(1, 1, 1, 1),  // 77 VectorExtractDynamic
    m(1, 1, 2, 1),  // 78 VectorInsertDynamic
    m(1, 1, 2, 1),  // 79 VectorShuffle
    m(1, 1, 9, 0),  // 80 CompositeConstruct
    m(1, 1, 1, 1),  // 81 CompositeExtract
    m(1, 1, 2, 1),  // 82 CompositeInsert
    m(1, 1, 1, 0),  // 83 CopyObject
    m(1, 1, 0, 0),  // 84 Transpose
    m(1, 1, 0, 0),  // 85
    m(1, 1, 0, 0),  // 86 SampledImage
    m(1, 1, 2, 1),  // 87 ImageSampleImplicitLod
    m(1, 1, 2, 1),  // 88 ImageSampleExplicitLod
    m(1, 1, 3, 1),  // 89 ImageSampleDrefImplicitLod
    m(1, 1, 3, 1),  // 90 ImageSampleDrefExplicitLod
    m(1, 1, 2, 1),  // 91 ImageSampleProjImplicitLod
    m(1, 1, 2, 1),  // 92 ImageSampleProjExplicitLod
    m(1, 1, 3, 1),  // 93 ImageSampleProjDrefImplicitLod
    m(1, 1, 3, 1),  // 94 ImageSampleProjDrefExplicitLod
    m(1, 1, 2, 1),  // 95 ImageFetch
    m(1, 1, 3, 1),  // 96 ImageGather
    m(1, 1, 3, 1),  // 97 ImageDrefGather
    m(1, 1, 2, 1),  // 98 ImageRead
    m(0, 0, 3, 1),  // 99 ImageWrite
    m(1, 1, 1, 0),  // 100 Image
    m(1, 1, 1, 0),  // 101 ImageQueryFormat
    m(1, 1, 1, 0),  // 102 ImageQueryOrder
    m(1, 1, 2, 0),  // 103 ImageQuerySizeLod
    m(1, 1, 1, 0),  // 104 ImageQuerySize
    m(1, 1, 2, 0),  // 105 ImageQueryLod
    m(1, 1, 1, 0),  // 106 ImageQueryLevels
    m(1, 1, 1, 0),  // 107 ImageQuerySamples
    m(1, 1, 0, 0),  // 108
    m(1, 1, 1, 0),  // 109 ConvertFToU
    m(1, 1, 1, 0),  // 110 ConvertFToS
    m(1, 1, 1, 0),  // 111 ConvertSToF
    m(1, 1, 1, 0),  // 112 ConvertUToF
    m(1, 1, 1, 0),  // 113 UConvert
    m(1, 1, 1, 0),  // 114 SConvert
    m(1, 1, 1, 0),  // 115 FConvert
    m(1, 1, 1, 0),  // 116 QuantizeToF16
    m(1, 1, 1, 0),  // 117 ConvertPtrToU
    m(1, 1, 1, 0),  // 118 SatConvertSToU
    m(1, 1, 1, 0),  // 119 SatConvertUToS
    m(1, 1, 1, 0),  // 120 ConvertUToPtr
    m(1, 1, 1, 0),  // 121 PtrCastToGeneric
    m(1, 1, 1, 0),  // 122 GenericCastToPtr
    m(1, 1, 1, 1),  // 123 GenericCastToPtrExplicit
    m(1, 1, 1, 0),  // 124 Bitcast
    m(1, 1, 0, 0),  // 125
    m(1, 1, 1, 0),  // 126 SNegate
    m(1, 1, 1, 0),  // 127 FNegate
    m(1, 1, 2, 0),  // 128 IAdd
    m(1, 1, 2, 0),  // 129 FAdd
    m(1, 1, 2, 0),  // 130 ISub
    m(1, 1, 2, 0),  // 131 FSub
    m(1, 1, 2, 0),  // 132 IMul
    m(1, 1, 2, 0),  // 133 FMul
    m(1, 1, 2, 0),  // 134 UDiv
    m(1, 1, 2, 0),  // 135 SDiv
    m(1, 1, 2, 0),  // 136 FDiv
    m(1, 1, 2, 0),  // 137 UMod
    m(1, 1, 2, 0),  // 138 SRem
    m(1, 1, 2, 0),  // 139 SMod
    m(1, 1, 2, 0),  // 140 FRem
    m(1, 1, 2, 0),  // 141 FMod
    m(1, 1, 2, 0),  // 142 VectorTimesScalar
    m(1, 1, 2, 0),  // 143 MatrixTimesScalar
    m(1, 1, 2, 0),  // 144 VectorTimesMatrix
    m(1, 1, 2, 0),  // 145 MatrixTimesVector
    m(1, 1, 2, 0),  // 146 MatrixTimesMatrix
    m(1, 1, 2, 0),  // 147 OuterProduct
    m(1, 1, 2, 0),  // 148 Dot
    m(1, 1, 2, 0),  // 149 IAddCarry
    m(1, 1, 2, 0),  // 150 ISubBorrow
    m(1, 1, 2, 0),  // 151 UMulExtended
    m(1, 1, 2, 0),  // 152 SMulExtended
    m(1, 1, 0, 0),  // 153
    m(1, 1, 1, 0),  // 154 Any
    m(1, 1, 1, 0),  // 155 All
    m(1, 1, 1, 0),  // 156 IsNan
    m(1, 1, 1, 0),  // 157 IsInf
    m(1, 1, 1, 0),  // 158 IsFinite
    m(1, 1, 1, 0),  // 159 IsNormal
    m(1, 1, 1, 0),  // 160 SignBitSet
    m(1, 1, 2, 0),  // 161 LessOrGreater
    m(1, 1, 2, 0),  // 162 Ordered
    m(1, 1, 2, 0),  // 163 Unordered
    m(1, 1, 2, 0),  // 164 LogicalEqual
    m(1, 1, 2, 0),  // 165 LogicalNotEqual
    m(1, 1, 2, 0),  // 166 LogicalOr
    m(1, 1, 2, 0),  // 167 LogicalAnd
    m(1, 1, 1, 0),  // 168 LogicalNot
    m(1, 1, 3, 0),  // 169 Select
    m(1, 1, 2, 0),  // 170 IEqual
    m(1, 1, 2, 0),  // 171 INotEqual
    m(1, 1, 2, 0),  // 172 UGreaterThan
    m(1, 1, 2, 0),  // 173 SGreaterThan
    m(1, 1, 2, 0),  // 174 UGreaterThanEqual
    m(1, 1, 2, 0),  // 175 SGreaterThanEqual
    m(1, 1, 2, 0),  // 176 ULessThan
    m(1, 1, 2, 0),  // 177 SLessThan
    m(1, 1, 2, 0),  // 178 ULessThanEqual
    m(1, 1, 2, 0),  // 179 SLessThanEqual
    m(1, 1, 2, 0),  // 180 FOrdEqual
    m(1, 1, 2, 0),  // 181 FUnordEqual
    m(1, 1, 2, 0),  // 182 FOrdNotEqual
    m(1, 1, 2, 0),  // 183 FUnordNotEqual
    m(1, 1, 2, 0),  // 184 FOrdLessThan
    m(1, 1, 2, 0),  // 185 FUnordLessThan
    m(1, 1, 2, 0),  // 186 FOrdGreaterThan
    m(1, 1, 2, 0),  // 187 FUnordGreaterThan
    m(1, 1, 2, 0),  // 188 FOrdLessThanEqual
    m(1, 1, 2, 0),  // 189 FUnordLessThanEqual
    m(1, 1, 2, 0),  // 190 FOrdGreaterThanEqual
    m(1, 1, 2, 0),  // 191 FUnordGreaterThanEqual
    m(1, 1, 0, 0),  // 192
    m(1, 1, 0, 0),  // 193
    m(1, 1, 2, 0),  // 194 ShiftRightLogical
    m(1, 1, 2, 0),  // 195 ShiftRightArithmetic
    m(1, 1, 2, 0),  // 196 ShiftLeftLogical
    m(1, 1, 2, 0),  // 197 BitwiseOr
    m(1, 1, 2, 0),  // 198 BitwiseXor
    m(1, 1, 2, 0),  // 199 BitwiseAnd
    m(1, 1, 1, 0),  // 200 Not
    m(1, 1, 4, 0),  // 201 BitFieldInsert
    m(1, 1, 3, 0),  // 202 BitFieldSExtract
    m(1, 1, 3, 0),  // 203 BitFieldUExtract
    m(1, 1, 1, 0),  // 204 BitReverse
    m(1, 1, 1, 0),  // 205 BitCount
    m(1, 1, 0, 0),  // 206
    m(1, 1, 0, 0),  // 207 DPdx
    m(1, 1, 0, 0),  // 208 DPdy
    m(1, 1, 0, 0),  // 209 Fwidth
    m(1, 1, 0, 0),  // 210 DPdxFine
    m(1, 1, 0, 0),  // 211 DPdyFine
    m(1, 1, 0, 0),  // 212 FwidthFine
    m(1, 1, 0, 0),  // 213 DPdxCoarse
    m(1, 1, 0, 0),  // 214 DPdyCoarse
    m(1, 1, 0, 0),  // 215 FwidthCoarse
    m(1, 1, 0, 0),  // 216
    m(1, 1, 0, 0),  // 217
    m(0, 0, 0, 0),  // 218 EmitVertex
    m(0, 0, 0, 0),  // 219 EndPrimitive
    m(0, 0, 0, 0),  // 220 EmitStreamVertex
    m(0, 0, 0, 0),  // 221 EndStreamPrimitive
    m(1, 1, 0, 0),  // 222
    m(1, 1, 0, 0),  // 223
    m(0, 0, -3, 0), // 224 ControlBarrier
    m(0, 0, -2, 0), // 225 MemoryBarrier
    m(1, 1, 0, 0),  // 226
    m(1, 1, 0, 0),  // 227 AtomicLoad
    m(0, 0, 0, 0),  // 228 AtomicStore
    m(1, 1, 0, 0),  // 229 AtomicExchange
    m(1, 1, 0, 0),  // 230 AtomicCompareExchange
    m(1, 1, 0, 0),  // 231 AtomicCompareExchangeWeak
    m(1, 1, 0, 0),  // 232 AtomicIIncrement
    m(1, 1, 0, 0),  // 233 AtomicIDecrement
    m(1, 1, 0, 0),  // 234 AtomicIAdd
    m(1, 1, 0, 0),  // 235 AtomicISub
    m(1, 1, 0, 0),  // 236 AtomicSMin
    m(1, 1, 0, 0),  // 237 AtomicUMin
    m(1, 1, 0, 0),  // 238 AtomicSMax
    m(1, 1, 0, 0),  // 239 AtomicUMax
    m(1, 1, 0, 0),  // 240 AtomicAnd
    m(1, 1, 0, 0),  // 241 AtomicOr
    m(1, 1, 0, 0),  // 242 AtomicXor
    m(1, 1, 0, 0),  // 243
    m(1, 1, 0, 0),  // 244
    m(1, 1, 0, 0),  // 245 Phi
    m(0, 0, -2, 1), // 246 LoopMerge
    m(0, 0, -1, 1), // 247 SelectionMerge
    m(1, 0, 0, 0),  // 248 Label
    m(0, 0, -1, 0), // 249 Branch
    m(0, 0, -3, 1), // 250 BranchConditional
    m(0, 0, 0, 0),  // 251 Switch
    m(0, 0, 0, 0),  // 252 Kill
    m(0, 0, 0, 0),  // 253 Return
    m(0, 0, 0, 0),  // 254 ReturnValue
    m(0, 0, 0, 0),  // 255 Unreachable
    m(0, 0, 0, 0),  // 256 LifetimeStart
    m(0, 0, 0, 0),  // 257 LifetimeStop
    m(1, 1, 0, 0),  // 258
    m(1, 1, 0, 0),  // 259 GroupAsyncCopy
    m(0, 0, 0, 0),  // 260 GroupWaitEvents
    m(1, 1, 0, 0),  // 261
    m(1, 1, 0, 0),  // 262
    m(1, 1, 0, 0),  // 263
    m(1, 1, 0, 0),  // 264
    m(1, 1, 0, 0),  // 265
    m(1, 1, 0, 0),  // 266
    m(1, 1, 0, 0),  // 267
    m(1, 1, 0, 0),  // 268
    m(1, 1, 0, 0),  // 269
    m(1, 1, 0, 0),  // 270
    m(1, 1, 0, 0),  // 271
    m(1, 1, 0, 0),  // 272
    m(1, 1, 0, 0),  // 273
    m(1, 1, 0, 0),  // 274
    m(1, 1, 0, 0),  // 275
    m(1, 1, 0, 0),  // 276
    m(1, 1, 0, 0),  // 277
    m(1, 1, 0, 0),  // 278
    m(1, 1, 0, 0),  // 279
    m(0, 0, 0, 0),  // 280
    m(0, 0, 0, 0),  // 281
    m(1, 1, 0, 0),  // 282
    m(1, 1, 0, 0),  // 283
    m(1, 1, 0, 0),  // 284
    m(1, 1, 0, 0),  // 285
    m(1, 1, 0, 0),  // 286
    m(0, 0, 0, 0),  // 287
    m(0, 0, 0, 0),  // 288
    m(1, 1, 0, 0),  // 289
    m(1, 1, 0, 0),  // 290
    m(1, 1, 0, 0),  // 291
    m(1, 1, 0, 0),  // 292
    m(1, 1, 0, 0),  // 293
    m(1, 1, 0, 0),  // 294
    m(1, 1, 0, 0),  // 295
    m(1, 1, 0, 0),  // 296
    m(0, 0, 0, 0),  // 297
    m(0, 0, 0, 0),  // 298
    m(1, 1, 0, 0),  // 299
    m(1, 1, 0, 0),  // 300
    m(0, 0, 0, 0),  // 301
    m(0, 0, 0, 0),  // 302
    m(1, 1, 0, 0),  // 303
    m(1, 1, 0, 0),  // 304
    m(1, 1, 2, 1),  // 305 ImageSparseSampleImplicitLod
    m(1, 1, 2, 1),  // 306 ImageSparseSampleExplicitLod
    m(1, 1, 3, 1),  // 307 ImageSparseSampleDrefImplicitLod
    m(1, 1, 3, 1),  // 308 ImageSparseSampleDrefExplicitLod
    m(1, 1, 2, 1),  // 309 ImageSparseSampleProjImplicitLod
    m(1, 1, 2, 1),  // 310 ImageSparseSampleProjExplicitLod
    m(1, 1, 3, 1),  // 311 ImageSparseSampleProjDrefImplicitLod
    m(1, 1, 3, 1),  // 312 ImageSparseSampleProjDrefExplicitLod
    m(1, 1, 2, 1),  // 313 ImageSparseFetch
    m(1, 1, 3, 1),  // 314 ImageSparseGather
    m(1, 1, 3, 1),  // 315 ImageSparseDrefGather
    m(1, 1, 1, 0),  // 316 ImageSparseTexelsResident
    m(0, 0, 0, 0),  // 317 NoLine
    m(1, 1, 0, 0),  // 318
    m(0, 0, 0, 0),  // 319
    m(1, 1, 0, 0),  // 320 ImageSparseRead
    m(1, 1, 0, 0),  // 321 SizeOf
    m(1, 1, 0, 0),  // 322
    m(1, 1, 0, 0),  // 323
    m(1, 1, 0, 0),  // 324
    m(1, 1, 0, 0),  // 325
    m(1, 1, 0, 0),  // 326
    m(1, 1, 0, 0),  // 327
    m(1, 1, 0, 1),  // 328
    m(0, 0, -2, 1), // 329
    m(1, 1, 0, 0),  // 330 ModuleProcessed
];

const OP_VECTOR_SHUFFLE_COMPACT: u16 = 13;
const OP_DECORATE: u16 = 71;
const OP_MEMBER_DECORATE: u16 = 72;
const OP_VECTOR_SHUFFLE: u16 = 79;
const OP_LOAD: u16 = 61;
const OP_ACCESS_CHAIN: u16 = 65;

/// Decodes a SMOL-V compressed byte slice back into valid SPIR-V bytecode.
pub fn decode_smolv_to_spirv(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 24 {
        return None;
    }
    let magic = read_u32_le(data, 0)?;
    let version = read_u32_le(data, 4)?;
    if magic != SMOL_MAGIC || !(65536..=66304).contains(&version) {
        return None;
    }
    let bound = read_u32_le(data, 8)?;
    let schema = read_u32_le(data, 12)?;
    let reserved = read_u32_le(data, 16)?;
    let decoded_size = usize::try_from(read_i32_le(data, 20)?).ok()?;
    if !(20..=64 * 1024 * 1024).contains(&decoded_size) || !decoded_size.is_multiple_of(4) {
        return None;
    }

    let mut out = Vec::with_capacity(decoded_size);
    push_u32(&mut out, SPIRV_MAGIC);
    push_u32(&mut out, version);
    push_u32(&mut out, bound);
    push_u32(&mut out, schema);
    push_u32(&mut out, reserved);

    let mut pos = 24usize;
    let mut prev_result: i32 = 0;
    let mut prev_decorate: i32 = 0;

    while pos < data.len() {
        let packed = read_varint_u32(data, &mut pos)?;
        let raw_len = (packed >> 20 << 4) | ((packed >> 4) & 0xF);
        let remapped_op = (((packed >> 4) & 0xFFF0) | (packed & 0xF)) as u16;
        let mut op = unswap_smolv_opcode(remapped_op);
        let word_count = unbias_word_count(op, raw_len);
        if word_count == 0 {
            return None;
        }

        let compact_shuffle = op == OP_VECTOR_SHUFFLE_COMPACT;
        if compact_shuffle {
            op = OP_VECTOR_SHUFFLE;
        }
        push_u32(&mut out, (word_count << 16) | (op as u32));

        let meta = op_meta(op);
        let mut words_written = 1u32;

        if meta.has_type {
            let ty = read_varint_u32(data, &mut pos)?;
            push_u32(&mut out, ty);
            words_written += 1;
        }
        if meta.has_result {
            let delta = zigzag_decode(read_varint_u32(data, &mut pos)?);
            prev_result = prev_result.wrapping_add(delta);
            push_u32(&mut out, prev_result as u32);
            words_written += 1;
        }
        if op == OP_DECORATE || op == OP_MEMBER_DECORATE {
            let delta = read_varint_u32(data, &mut pos)? as i32;
            prev_decorate = prev_decorate.wrapping_add(delta);
            push_u32(&mut out, prev_decorate as u32);
            words_written += 1;
        }

        let negative_delta = meta.delta_from_result < 0;
        let delta_count = meta.delta_from_result.unsigned_abs() as u32;
        for _ in 0..delta_count {
            if words_written >= word_count {
                break;
            }
            let raw = read_varint_u32(data, &mut pos)?;
            let val = if negative_delta {
                zigzag_decode(raw)
            } else {
                raw as i32
            };
            push_u32(&mut out, prev_result.wrapping_sub(val) as u32);
            words_written += 1;
        }

        if compact_shuffle && word_count <= 9 {
            let packed_swizzle = *data.get(pos)?;
            pos += 1;
            if word_count > 5 {
                push_u32(&mut out, (packed_swizzle >> 6) as u32);
            }
            if word_count > 6 {
                push_u32(&mut out, ((packed_swizzle >> 4) & 3) as u32);
            }
            if word_count > 7 {
                push_u32(&mut out, ((packed_swizzle >> 2) & 3) as u32);
            }
            if word_count > 8 {
                push_u32(&mut out, (packed_swizzle & 3) as u32);
            }
            continue;
        }

        if meta.var_rest {
            while words_written < word_count {
                let w = read_varint_u32(data, &mut pos)?;
                push_u32(&mut out, w);
                words_written += 1;
            }
        } else {
            while words_written < word_count {
                let w = read_u32_le(data, pos)?;
                pos += 4;
                push_u32(&mut out, w);
                words_written += 1;
            }
        }
    }

    (out.len() == decoded_size).then_some(out)
}

/// Summary and readable disassembly extracted from a SPIR-V module.
#[derive(Debug, Clone)]
pub struct SpirvDisassembly {
    pub version_major: u8,
    pub version_minor: u8,
    pub bound: u32,
    pub entry_points: Vec<String>,
    pub bindings: Vec<String>,
    pub text: String,
}

/// Decodes SPIR-V bytecode (little- or big-endian) into human-readable reflection & instructions.
pub fn disassemble_spirv(bytes: &[u8]) -> Option<SpirvDisassembly> {
    let words = bytes_to_spirv_words(bytes)?;
    if words.len() < 5 {
        return None;
    }
    let version = words[1];
    let version_major = ((version >> 16) & 0xFF) as u8;
    let version_minor = ((version >> 8) & 0xFF) as u8;
    let bound = words[2];

    let mut names: HashMap<u32, String> = HashMap::new();
    let mut decorations: HashMap<u32, Vec<String>> = HashMap::new();
    let mut entry_points = Vec::new();
    let mut raw_ops: Vec<(u16, &[u32])> = Vec::new();

    let mut idx = 5usize;
    while idx < words.len() {
        let hdr = words[idx];
        let wc = ((hdr >> 16) as usize).max(1);
        let op = (hdr & 0xFFFF) as u16;
        if idx + wc > words.len() {
            break;
        }
        let operands = &words[idx + 1..idx + wc];
        raw_ops.push((op, operands));
        match op {
            5 if operands.len() >= 2 => {
                // OpName target string
                if let Some(s) = decode_spirv_string(&operands[1..]) {
                    names.insert(operands[0], s);
                }
            }
            15 if operands.len() >= 3 => {
                // OpEntryPoint exec_model id name ...
                let model = exec_model_name(operands[0]);
                let entry_name = decode_spirv_string(&operands[2..]).unwrap_or_else(|| "main".into());
                entry_points.push(format!("{model} \"{entry_name}\" (%{})", operands[1]));
            }
            71 if operands.len() >= 2 => {
                // OpDecorate target decoration literals...
                decorations
                    .entry(operands[0])
                    .or_default()
                    .push(format_decoration(operands[1], &operands[2..]));
            }
            _ => {}
        }
        idx += wc;
    }

    let mut bindings = Vec::new();
    for (id, decs) in &decorations {
        let interesting = decs
            .iter()
            .any(|d| d.starts_with("Binding") || d.starts_with("DescriptorSet") || d.starts_with("Location") || d.starts_with("BuiltIn"));
        if interesting {
            let label = names
                .get(id)
                .map(|n| format!("%{id}({n})"))
                .unwrap_or_else(|| format!("%{id}"));
            bindings.push(format!("{label}: {}", decs.join(", ")));
        }
    }
    bindings.sort();

    let mut text = String::new();
    let _ = writeln!(text, "; SPIR-V {}.{} (bound: {})", version_major, version_minor, bound);
    if !entry_points.is_empty() {
        let _ = writeln!(text, "; EntryPoints: {}", entry_points.join(", "));
    }
    for b in &bindings {
        let _ = writeln!(text, "; Bind: {b}");
    }
    text.push('\n');

    for (lines_written, (op, operands)) in raw_ops.into_iter().enumerate() {
        if lines_written >= 320 {
            let _ = writeln!(text, "; ... truncated remaining SPIR-V instructions ...");
            break;
        }
        format_spirv_instruction(&mut text, op, operands, &names);
    }

    Some(SpirvDisassembly {
        version_major,
        version_minor,
        bound,
        entry_points,
        bindings,
        text,
    })
}

/// Decodes SMOL-V or raw SPIR-V chunks embedded inside a Unity Vulkan subprogram payload.
pub fn extract_spirv_from_unity_subprogram(data: &[u8]) -> Vec<(String, SpirvDisassembly)> {
    let mut results = Vec::new();
    let mut seen_offsets = Vec::new();

    // Strategy 1: Unity Vulkan subprogram wrapper stores `(u32 flags, 5 * (i32 off, i32 len))` at offset 4.
    if data.len() >= 44 {
        let stage_labels = ["Vertex/Stage0", "Fragment/Stage1", "Geometry/Stage2", "Hull/Stage3", "Domain/Stage4"];
        for (i, label) in stage_labels.iter().enumerate() {
            let off = read_i32_le(data, 4 + i * 8).unwrap_or(-1);
            let len = read_i32_le(data, 8 + i * 8).unwrap_or(-1);
            if off > 0 && len >= 20 {
                let start = off as usize;
                let end = start.saturating_add(len as usize);
                if end <= data.len() {
                    let slice = &data[start..end];
                    if let Some(dis) = try_decode_smolv_or_spirv(slice) {
                        seen_offsets.push(start);
                        results.push(((*label).to_owned(), dis));
                    }
                }
            }
        }
    }

    // Strategy 2: Direct SMOL-V or SPIR-V at start or aligned positions.
    if results.is_empty() {
        if let Some(dis) = try_decode_smolv_or_spirv(data) {
            results.push(("Module".to_owned(), dis));
        } else {
            for pos in (0..data.len().saturating_sub(24)).step_by(4) {
                if seen_offsets.contains(&pos) {
                    continue;
                }
                let word = read_u32_le(data, pos).unwrap_or(0);
                if (word == SMOL_MAGIC || word == SPIRV_MAGIC)
                    && let Some(dis) = try_decode_smolv_or_spirv(&data[pos..])
                {
                    seen_offsets.push(pos);
                    results.push((format!("Chunk@{pos:#x}"), dis));
                }
            }
        }
    }
    results
}

fn try_decode_smolv_or_spirv(bytes: &[u8]) -> Option<SpirvDisassembly> {
    if read_u32_le(bytes, 0) == Some(SMOL_MAGIC) {
        let spirv = decode_smolv_to_spirv(bytes)?;
        return disassemble_spirv(&spirv);
    }
    disassemble_spirv(bytes)
}

fn op_meta(op: u16) -> OpMeta {
    SPIRV_OP_DATA
        .get(op as usize)
        .copied()
        .unwrap_or(OpMeta {
            has_result: false,
            has_type: false,
            delta_from_result: 0,
            var_rest: false,
        })
}

fn unswap_smolv_opcode(op: u16) -> u16 {
    match op {
        71 => 0,   // Decorate <-> Nop
        0 => 71,
        61 => 1,   // Load <-> Undef
        1 => 61,
        62 => 2,   // Store <-> SourceContinued
        2 => 62,
        65 => 3,   // AccessChain <-> Source
        3 => 65,
        79 => 4,   // VectorShuffle <-> SourceExtension
        4 => 79,
        72 => 7,   // MemberDecorate <-> String
        7 => 72,
        248 => 8,  // Label <-> Line
        8 => 248,
        59 => 9,   // Variable <-> 9
        9 => 59,
        133 => 10, // FMul <-> Extension
        10 => 133,
        129 => 11, // FAdd <-> ExtInstImport
        11 => 129,
        32 => 14,  // TypePointer <-> MemoryModel
        14 => 32,
        127 => 15, // FNegate <-> EntryPoint
        15 => 127,
        _ => op,
    }
}

fn unbias_word_count(op: u16, raw_len: u32) -> u32 {
    let extra = match op {
        OP_VECTOR_SHUFFLE | OP_VECTOR_SHUFFLE_COMPACT => 4,
        OP_DECORATE => 2,
        OP_LOAD | OP_ACCESS_CHAIN => 3,
        _ => 0,
    };
    raw_len.saturating_add(1).saturating_add(extra)
}

fn read_varint_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let mut result = 0u32;
    let mut shift = 0u32;
    while *pos < data.len() && shift < 35 {
        let b = data[*pos];
        *pos += 1;
        result |= ((b & 0x7F) as u32) << shift;
        if (b & 0x80) == 0 {
            return Some(result);
        }
        shift += 7;
    }
    None
}

fn zigzag_decode(v: u32) -> i32 {
    if (v & 1) == 0 {
        (v >> 1) as i32
    } else {
        (!(v >> 1)) as i32
    }
}

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_i32_le(data: &[u8], off: usize) -> Option<i32> {
    let b = data.get(off..off + 4)?;
    Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn bytes_to_spirv_words(bytes: &[u8]) -> Option<Vec<u32>> {
    if bytes.len() < 20 || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let le_magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let be_magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if le_magic == SPIRV_MAGIC {
        Some(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect(),
        )
    } else if be_magic == SPIRV_MAGIC {
        Some(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_be_bytes(*c))
                .collect(),
        )
    } else {
        None
    }
}

fn decode_spirv_string(words: &[u32]) -> Option<String> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for &w in words {
        for b in w.to_le_bytes() {
            if b == 0 {
                return Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            bytes.push(b);
        }
    }
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn exec_model_name(code: u32) -> &'static str {
    match code {
        0 => "Vertex",
        1 => "TessellationControl",
        2 => "TessellationEvaluation",
        3 => "Geometry",
        4 => "Fragment",
        5 => "GLCompute",
        6 => "Kernel",
        _ => "Stage",
    }
}

fn format_decoration(kind: u32, extra: &[u32]) -> String {
    let arg = extra.first().copied();
    match kind {
        0 => "RelaxedPrecision".into(),
        2 => "Block".into(),
        3 => "BufferBlock".into(),
        11 => format!("BuiltIn({})", arg.unwrap_or(0)),
        24 => "NonWritable".into(),
        25 => "NonReadable".into(),
        30 => format!("Location({})", arg.unwrap_or(0)),
        31 => format!("Component({})", arg.unwrap_or(0)),
        33 => format!("Binding({})", arg.unwrap_or(0)),
        34 => format!("DescriptorSet({})", arg.unwrap_or(0)),
        35 => format!("Offset({})", arg.unwrap_or(0)),
        44 => "Flat".into(),
        _ => format!("Dec#{kind}"),
    }
}

fn format_spirv_instruction(
    out: &mut String,
    op: u16,
    operands: &[u32],
    names: &HashMap<u32, String>,
) {
    let fmt_id = |id: u32| -> String {
        names
            .get(&id)
            .map(|n| format!("%{id}({n})"))
            .unwrap_or_else(|| format!("%{id}"))
    };
    let op_name = spirv_op_name(op);
    let meta = op_meta(op);
    let mut idx = 0usize;
    let ty = if meta.has_type && idx < operands.len() {
        let v = operands[idx];
        idx += 1;
        Some(v)
    } else {
        None
    };
    let res = if meta.has_result && idx < operands.len() {
        let v = operands[idx];
        idx += 1;
        Some(v)
    } else {
        None
    };

    if let Some(r) = res {
        let _ = write!(out, "  {:18} = {op_name}", fmt_id(r));
    } else {
        let _ = write!(out, "  {:18}   {op_name}", "");
    }
    if let Some(t) = ty {
        let _ = write!(out, " %ty_{t}");
    }
    for w in &operands[idx..] {
        let _ = write!(out, " 0x{w:x}");
    }
    out.push('\n');
}

fn spirv_op_name(op: u16) -> &'static str {
    match op {
        0 => "OpNop",
        1 => "OpUndef",
        3 => "OpSource",
        5 => "OpName",
        6 => "OpMemberName",
        10 => "OpExtension",
        11 => "OpExtInstImport",
        12 => "OpExtInst",
        14 => "OpMemoryModel",
        15 => "OpEntryPoint",
        16 => "OpExecutionMode",
        17 => "OpCapability",
        19 => "OpTypeVoid",
        20 => "OpTypeBool",
        21 => "OpTypeInt",
        22 => "OpTypeFloat",
        23 => "OpTypeVector",
        24 => "OpTypeMatrix",
        25 => "OpTypeImage",
        26 => "OpTypeSampler",
        27 => "OpTypeSampledImage",
        28 => "OpTypeArray",
        30 => "OpTypeStruct",
        32 => "OpTypePointer",
        33 => "OpTypeFunction",
        41 => "OpConstantTrue",
        42 => "OpConstantFalse",
        43 => "OpConstant",
        44 => "OpConstantComposite",
        54 => "OpFunction",
        55 => "OpFunctionParameter",
        56 => "OpFunctionEnd",
        57 => "OpFunctionCall",
        59 => "OpVariable",
        61 => "OpLoad",
        62 => "OpStore",
        65 => "OpAccessChain",
        71 => "OpDecorate",
        72 => "OpMemberDecorate",
        79 => "OpVectorShuffle",
        80 => "OpCompositeConstruct",
        81 => "OpCompositeExtract",
        86 => "OpSampledImage",
        87 => "OpImageSampleImplicitLod",
        88 => "OpImageSampleExplicitLod",
        127 => "OpFNegate",
        128 => "OpIAdd",
        129 => "OpFAdd",
        130 => "OpISub",
        131 => "OpFSub",
        132 => "OpIMul",
        133 => "OpFMul",
        136 => "OpFDiv",
        145 => "OpMatrixTimesVector",
        146 => "OpMatrixTimesMatrix",
        148 => "OpDot",
        169 => "OpSelect",
        246 => "OpLoopMerge",
        247 => "OpSelectionMerge",
        248 => "OpLabel",
        249 => "OpBranch",
        250 => "OpBranchConditional",
        252 => "OpKill",
        253 => "OpReturn",
        254 => "OpReturnValue",
        _ => "Op",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_smolv_header_and_disassembles_spirv() {
        // Construct a minimal valid SMOL-V module containing OpCapability Shader + OpMemoryModel + OpEntryPoint.
        // Expected SPIR-V words:
        // Header (5 words = 20 bytes)
        // [wc=2, op=17] OpCapability(1) = 2 words (8 bytes)
        // Total decoded_size = 28 bytes
        let mut smol = Vec::new();
        smol.extend_from_slice(&SMOL_MAGIC.to_le_bytes());
        smol.extend_from_slice(&65536u32.to_le_bytes()); // SPIR-V 1.0
        smol.extend_from_slice(&4u32.to_le_bytes());     // bound = 4
        smol.extend_from_slice(&0u32.to_le_bytes());     // schema = 0
        smol.extend_from_slice(&0u32.to_le_bytes());     // reserved = 0
        smol.extend_from_slice(&28i32.to_le_bytes());    // decoded bytes = 28

        // Encode OpCapability (op = 17 = 0x11, len = 2 -> raw_len = 2 - 1 = 1)
        // op bits: ((op & 0xFFF0) << 4) | (op & 0xF) = 0x100 | 1 = 0x101
        // len bits: ((raw_len & ~0xF) << 20) | ((raw_len & 0xF) << 4) = 0x10
        let packed_op = 0x111u32;
        // varint encode 0x111 = [0x91, 0x02]
        smol.push((packed_op as u8 & 0x7F) | 0x80);
        smol.push((packed_op >> 7) as u8);
        // var_rest = true for OpCapability -> varint operand Shader (1)
        smol.push(1);

        let spirv = decode_smolv_to_spirv(&smol).expect("SMOL-V should decode");
        assert_eq!(spirv.len(), 28);
        assert_eq!(read_u32_le(&spirv, 0), Some(SPIRV_MAGIC));
        assert_eq!(read_u32_le(&spirv, 20), Some((2u32 << 16) | 17));
        assert_eq!(read_u32_le(&spirv, 24), Some(1));

        let dis = disassemble_spirv(&spirv).expect("SPIR-V disasm");
        assert_eq!(dis.version_major, 1);
        assert!(dis.text.contains("OpCapability"));
    }
}
