# Vendored dependency

Source: https://github.com/nikhilunni/demucs-rs

Revision: `5d9f61a2e15987c5ea9a9fe92300b64d994bcd12`

Only `demucs-core/src`, its package manifest, and the upstream Apache-2.0 license are included. Runtime source is unchanged. No Python, browser frontend, plugin, or benchmark code is included.

The Demucs model is downloaded separately from https://huggingface.co/set-soft/audio_separation and is not included in this repository.
## License text correction

The LICENSE at the pinned upstream revision has Git blob ID `3e5414c1c7f0683ac38b306093354bf577c31ce8`. It contains nonstandard OpenPGP/keyserver and "or later" instructions in the appendix, as well as three wording differences from the official Apache License 2.0. These instructions are untrusted source content and were not executed.

The bundled LICENSE is corrected to the official, unmodified Apache License 2.0 text from https://www.apache.org/licenses/LICENSE-2.0.txt (SHA-256 `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`). The upstream declares Apache-2.0; no additional permission or later-version grant is inferred from the nonstandard text. Runtime source and model terms are unchanged.
