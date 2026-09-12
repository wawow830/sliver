# Offline complete-frame marker codec (experimental v0)

**Diagnostic plumbing, not a release or optical contract.** Implemented after the 2026-09-12 continuation request at the [planned complete-frame pixel-identity seam](native-performance-acceptance-proposal.md#identity-and-accounting). No production runtime, broker wire format, canonical fixture, native threshold or service changes are included.

## Interface

The uninstalled, standard-library-only module `scripts/native-performance-marker.py` exposes:

- `stamp(pixels, identity, *, stride=8032) -> bytes`: return a new logical frame with the marker band replaced. Input pixels are not mutated; all pixels outside the band and all row padding are preserved.
- `decode(pixels, *, stride=None, layout="logical") -> identity`: independently recover identity **from the supplied pixels**, without a producer identity argument. `stride=None` selects the tight stride for the declared layout. There is no geometry search, rotation repair, thresholding or error correction.

Both functions raise `ValueError` for unsupported or inconsistent input. `pixels` must be immutable `bytes` containing exactly `stride * height` bytes, not a live memory view, partial crop, file path or mutable shared frame. Stride must be an integer, at least `width * 4`, divisible by four, with total frame storage at most 16 MiB. Booleans and floating-point strides are rejected. Passing `stride=None` to `stamp` also selects its tight logical stride.

Identity has exactly the [observation reader's](native-performance-observation-format.md) four marker fields: `run_id`, `generation`, `frame_id`, `input_id`. Run/generation/input strings are 1–128 ASCII letters, digits, `_`, `-` or `.`; input alone may be null. Frame ID is an integer in `[1, 2^63−1]`, not a boolean or float. Empty input is not an alias for null. IDs are encoded in full, never truncated or hashed. Generation sequencing and causal receipt/order remain the reader's responsibility.

The module performs no file, process, clock, service, device or network IO and has no CLI. Run its offline seam tests with:

```sh
python3 -B scripts/test-native-performance-marker.py
python3 -O -B scripts/test-native-performance-marker.py
```

### Lua canvas encoder

`scripts/native-performance-marker.lua` is a separate uninstalled encoder, exposing `marker.draw(canvas, identity)`. Tests copy it into a temporary fixture directory and require it as an ordinary pure-Lua module. It draws the same wire packet through existing `canvas:raw_pixels`, not a new public canvas operation. It saves/restores canvas state, sets alpha to 1 and source compositing, and uses nearest filtering. The caller must restore an **identity transform and unrestricted clip** before this final draw. In Lua, absent/nil `input_id` means null; false, empty strings, floating-point frame IDs and metatable-bearing identity tables are rejected rather than coerced.

Three integration tests in `crates/sliverd/src/lua_integration_tests.rs` exercise the existing in-process Lua worker/canvas test path and **fake hardware**, not production worker/broker IPC: every pixel against the worked wire vector, full-capacity IDs including causal input and maximum integer, and malformed identities rejected without presentation. Both Python and Lua agree with the same independently evaluated vectors, rather than merely agreeing with each other's output. No generated fixture is applied to Sliver or the physical panel.

## Pixel geometry and packet

Supported memory is four bytes per pixel, little-endian Cairo ARGB32 (B, G, R, A). Marker pixels are **exact opaque black** `00 00 00 ff` or **exact opaque white** `ff ff ff ff`.

- **Logical:** 2008 × 60, marker rectangle `x=[188,1820)`, `y=[4,12)`, 1632 × 8 pixels.
- Each bit occupies a 2 × 2 cell. There are 816 columns and four rows of cells, holding exactly 3264 bits / 408 bytes. Cells are read left-to-right, top-to-bottom, most-significant bit first within each byte; white is 1.
- **Panel:** 60 × 2008, explicitly requested with `layout="panel"`. Pixel indices map `(x,y) -> (59-y,x)`, the clockwise pixel-centre mapping of `paint_logical_frame`'s `translate(60,0); rotate(pi/2)` and nearest filter in `crates/sliverd/src/m2_hardware.rs`. Tight stride is 240; a 256-byte scanline is also supported. Padding is not marker content.
- `stamp` writes only logical frames. A future renderer/adapter performs its usual rotation; the decoder can read either declared memory layout. No DRM modes, connectors or registers are queried.

| Byte offsets (half-open) | Encoding |
| --- | --- |
| `[0,8)` | ASCII magic/version `SLVMRK00` |
| `[8,16)` | Frame ID, unsigned big-endian representation, constrained to positive signed-64-bit range |
| `16`, `17`, `18` | Run, generation, input string lengths (one byte each); zero input length means null |
| `19` | Reserved, must be zero |
| `[20,148)` | Run identity bytes, zero-padded to 128 bytes |
| `[148,276)` | Generation identity bytes, zero-padded to 128 bytes |
| `[276,404)` | Input identity bytes, zero-padded to 128 bytes |
| `[404,408)` | CRC-32/ISO-HDLC of bytes `[0,404)`, unsigned big-endian (`zlib.crc32`) |

All four pixels of every cell must agree, including all channels and opacity. The decoder rejects nonbinary/nonopaque/nonuniform cells, CRC mismatch, unknown magic/version/flags, invalid lengths/characters/ranges and nonzero padding—even if a malformed packet has a recomputed valid CRC. It does not turn an undecodable frame into an observation with a guessed or absent input identity.

### Worked vector

For `{run_id: "r", generation: "g", frame_id: 1, input_id: null}`, the packet starts with hexadecimal `534c564d524b3030 0000000000000001 01010000`, followed by `r` padded to 128 bytes, `g` padded to 128 bytes and 128 zero bytes. CRC is `aa94d712`. This checksum was independently evaluated bit-by-bit with reflected polynomial `0xedb88320` and initial/final XOR `0xffffffff`, rather than generated by the codec under test.

- Packet SHA-256: `03bffa2efe3f0602f2f7e0fb340555b013f5ae1ef772f0afe47576ea97245c64`
- Tight logical frame SHA-256 on an otherwise opaque-black background: `9e1dabfda3ae5c7cf56d4ffffc9794983124b2f81ff879275b4a64aa4fbff444`

A second full-capacity vector uses 128 `r` bytes, 128 `g` bytes, frame `2^63−1` and 128 `i` input bytes. Its CRC is `474654e2`, and packet SHA-256 is `95589d10dbfe50e235c1aaa16c7b0449c57e55f8585701a7877a8255d5a3a3e3`.

Tests use these literal vectors and a separate row-wise reference rasterizer, not just encode/decode round trips. A matrix-rotation reference checks panel layout and padding. A synthetic codec → observation-reader test retains timeout/late-response accounting, excludes recovery replay from unique updates, refuses stale run pixels, and ensures unrelated animation is not counted as causal input response.

## Limits and next integration

- This proves **digital marker consistency**, not source authenticity. CRC is accidental-corruption detection, not authentication, and covers only the marker packet—not the rest of the frame. An intact old marker or a deliberately forged valid packet can decode. A matching marker alone cannot establish coherent capture of the whole image, DMA retirement or which frame the panel displayed.
- No optical decoder or calibration exists. Camera resampling, brightness/colour changes, antialiasing, compression, cropping or a nonopaque alpha byte are unsupported. Black/white cells do not make the decoder an optical timing instrument.
- No encoder, runtime collector, journal, capture-lifetime/closure policy, or provenance authentication is installed. Python stamping is an offline reference; the Lua encoder is exercised only in fake-hardware tests. Neither implementation's allocations/execution time have been compared against an uninstrumented native run; suitability for native instrumentation is unproven.
- A future diagnostic fixture must integrate this encoder **after** other drawing, with an identity transform, unrestricted clip and pixel-aligned unfiltered cells; it must allocate identity before attempting render. Future observers must decode a stable complete-frame snapshot at the agreed seam, retain failures/loss, record independent call/clock metadata, and join with render/input records. The reader must continue checking run/generation/frame/input identity rather than trusting call order.
- A live collector must not call malformed marker output a clean release frame, infer timestamps from this packet, waive missing samples, or insert marker metadata into the production broker wire format. No hardware integration should occur before the outstanding acceptance/capture policy decisions.

The existing ≥59.5 native release gate and old failed evidence remain unchanged. The observation reader still reports `acceptance: "not_evaluated"`; this codec does not select a broker-return/optical release claim or any numeric budget.
