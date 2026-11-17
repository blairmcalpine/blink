# NVENC bindings

`mod.rs` holds hand-written Rust declarations for the part of the NVIDIA Video
Encoder API that blink calls. Nothing is generated at build time, so the build
needs no libclang, and no NVIDIA code or binary is vendored: `nvEncodeAPI64.dll`
comes from the user's installed driver at runtime.

## Source

- File: `include/ffnvcodec/nvEncodeAPI.h`
- Repository: <https://github.com/FFmpeg/nv-codec-headers>
- Tag: `n12.1.14.0`, commit `1889e62e2d35ff7aa9baca2bceb14f053785e6f1`
  (the file is byte-identical in `n12.1.14.1`)
- API version: `NVENCAPI_MAJOR_VERSION 12`, `NVENCAPI_MINOR_VERSION 1`
  (Video Codec SDK 12.1)
- Minimum driver (from that repository's README): 531.61 on Windows,
  530.41.03 on Linux

API 12.1 was chosen because it covers every NVENC feature blink uses and runs on
any driver from April 2023 onward. The driver reports its maximum supported API
version and blink refuses to start on anything older than 12.1.

## License

The header is not covered by the Video Codec SDK license agreement. It carries
its own MIT notice, which states it applies to that header file only:

```text
Copyright (c) 2010-2023 NVIDIA Corporation

Permission is hereby granted, free of charge, to any person
obtaining a copy of this software and associated documentation
files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use,
copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the software, and to permit persons to whom the
software is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.
```

## How the layouts were checked

Every struct is transcribed in full, reserved fields included. Its size and the
offsets of the fields blink uses are asserted at compile time in `mod.rs`. The
expected numbers come from:

```sh
clang --target=x86_64-pc-windows-msvc -fsyntax-only \
  -Xclang -fdump-record-layouts-complete layouts.c
```

where `layouts.c` includes the header (with a two-line `windows.h` stub
defining `GUID` and `RECT`) and declares one variable of each struct. The same
header compiled for the host with `offsetof`/`sizeof` gives identical numbers,
and the struct version macros (`NV_ENC_*_VER`) were printed from that build;
the unit tests compare against them.

Deliberate simplifications (sizes and alignments unchanged) are listed in the
module docs of `mod.rs`.

To move to a newer API version, re-transcribe from the matching
`nv-codec-headers` tag, rerun the layout dump, and update the assertions. Struct
versions and layouts change between minor versions (12.2 changed several), so
do not just bump the version constants.
