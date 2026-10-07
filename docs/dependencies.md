# 使用する版とライセンス

2026-10-07の検品CLIは追加のRust依存・モデルを使わない。JIZURAは公式ブラウザ版0.10.1、確認時commit `fc16bfe43ea4a6c25a21f1caf04d17326de14f00` を比較した。ツール本体はMIT、公式は出力物の商用利用を認めるが曲と歌詞の権利は別。[公式の利用条件](https://github.com/852wa/JIZURA#利用について出力物の権利とライセンス)。ツールを製品へ同梱せず、導入済みのChromeで操作する。外部のGoogle Fonts読み込みとプロジェクト保存に注意し、詳細を [jizura-comparison.md](jizura-comparison.md) に記録した。

確認日: 2026-10-04。音質・字幕同期の改善でort、realfft、windowsを追加した。Rustの依存はCargo.lockに固定し、ビルド・テストは --locked で行う。下表はcargo metadataから取得した直接依存（開発用を含む）の宣言で、ライセンス全文の代わりではない。

| 依存 | 固定した版 | crateのライセンス宣言 |
| --- | --- | --- |
| windows | 0.58.0 | MIT OR Apache-2.0 |
| realfft | 3.5.0 | MIT |
| ort | 2.0.0-rc.10 | MIT OR Apache-2.0 |
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| axum | 0.8.9 | MIT |
| burn | 0.20.1 | MIT OR Apache-2.0 |
| clap | 4.6.7 | MIT OR Apache-2.0 |
| demucs-core | 0.1.0 | Apache-2.0 (vendor/LICENSE) |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| hound | 3.5.1 | Apache-2.0 |
| http-body-util | 0.1.5 | MIT |
| open | 5.4.4 | MIT |
| pollster | 0.4.0 | Apache-2.0/MIT |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| tokio | 1.53.2 | MIT |
| tokio-tungstenite | 0.28.0 | MIT |
| tokio-util | 0.7.19 | MIT |
| tower | 0.5.3 | MIT |
| uuid | 1.26.1 | Apache-2.0 OR MIT |

## 音源分離と外部プログラム

標準はKimのMel-Band RoFormer。fp16のONNXは [Ryan5453/unblend](https://huggingface.co/Ryan5453/unblend) のrevision `30b749fd691312b1e4b3f8fe79df0b51cde836cd`、473,549,669 bytes、SHA-256 `7600639eee90a7e2732b71af90618defac0cacaca5ddfed7b783babfdf8474bf`。原作者の[モデルカード](https://huggingface.co/KimberleyJSN/melbandroformer)はMITを宣言する。変換は利用条件を変更しない。

比較用fp32は [santiquirozの変換](https://github.com/santiquiroz/port-bs-roformer-onnx/releases/tag/models-v1.0)、models-v1.0、931,054,408 bytes、SHA-256 `1b8afd7780d8a234527748821dee6bc746d346f2088751c12fd48e8c873f625a`。STFT/iSTFTはMITの参照実装と数式を確認してRustで実装した。smank変換953,292,899 bytes、SHA-256 `64a4f3bee48fbe7d971b23875adc924ed004c3533f49672592641dddc0f6f561`も評価したが不採用。

WindowsのONNX実行部品はMicrosoft.ML.OnnxRuntime.Gpu.Windows 1.24.4（MIT）。onnxruntime.dll SHA-256は `3b46571d12a9567791a42a2b2967a79c4e2e957aacdba09a2ddb4fb391707baa`。CUDA Runtime 12.8.90、cuBLAS 12.8.4.1、cuFFT 11.3.3.83、cuDNN 9.14.0.64はNVIDIAの公開再配布物を取得して公開SHA-256と照合した。アーカイブのNVIDIAライセンスを導入先へ保持。課金APIを使わずPC内推論に用いる。部品は `D:/CC/Tools/onnxruntime-cuda/1.24.4/runtime` にまとめ、CPU/GPUが同じDLLを使う。実行ファイルだけ他PCへコピーして全機能が動くとは扱わない。

字幕候補のwhisper.cpp 1.9.2はMIT。ggml-large-v3-turbo-q5_0.binは574,041,195 bytes。英語CTCは [onnx-community/wav2vec2-base-960h-ONNX](https://huggingface.co/onnx-community/wav2vec2-base-960h-ONNX) のrevision `729c1a6730fb549c20a1c73a3d3f96f11020225e`、95,212,816 bytes、SHA-256 `1d9a366c27b2966625cd5035ac3db8847c53bba617169912bb251b42975a3a22`。上流はApache-2.0。Windows OCRはOS機能を内部の字幕フレーム確認に使用。

公式Windows yt-dlp 2026.08.19は本人曲の内部取得に使用。コアはUnlicense、依存を含む配布exeはGPLv3+の条件。ダウンローダーを製品へ同梱しない。

DemucsのコードのMITとモデルの条件は異なる。作者は[モデルの条件について](https://github.com/facebookresearch/demucs/issues/327)研究目的に限ると説明している。配布ミラーのMIT表記だけで商用許可とせず、今回の比較評価に限定する。将来の受注にDemucsをそのまま使う前提を置かない。FT比較モデルは336,125,008 bytes、SHA-256 `255c2650d26537ce4887c9c4cf08c6d4896fad2fecc0b78dc5b875b117bcc575`。

- demucs-core: 上流の固定commitは `5d9f61a2e15987c5ea9a9fe92300b64d994bcd12`、Apache-2.0。`vendor/demucs-core/UPSTREAM.md` と同梱LICENSEに記録する。
- HTDemucsのsafetensors: `set-soft/audio_separation` の `Demucs/htdemucs.safetensors`、84,030,696 bytes。期待SHA-256は `8193504cdfb3943adaf039b8acb524a46e87ebf232c383ac7a32c80a6578423e`。配布元モデルカードの宣言はMIT、上流DemucsもMIT。配布元の宣言と原モデルの出所を記録し、新しいモデルへ交換する際は条件を再確認する。[モデルカード](https://huggingface.co/set-soft/audio_separation/blob/main/README.md)、[上流LICENSE](https://github.com/facebookresearch/demucs/blob/main/LICENSE)。モデルをZIPへ同梱しない。
- 制作PCのFFmpeg/ffprobe: `8.0.1-full_build-www.gyan.dev`。確認したビルドは `--enable-gpl --enable-version3`。外部実行ファイルとして呼び出し、本リポジトリや販売動画にプログラムを同梱しない。各PCの実行版・設定は `ffmpeg -version` で制作記録に残す。LGPL/GPLの違いは構成に依存する。[FFmpeg公式ライセンス案内](https://ffmpeg.org/legal.html)
- libass、libx264、libmp3lame、Rubber Bandは上記FFmpegビルドの機能として利用する。単独のライブラリの再配布は行わない。別のFFmpegビルドで利用するときはASS、日本語フォント、rubberband、H.264/AAC/MP3エンコードの動作を確認する。
- 標準フォント: Windowsに導入済みのYu Gothic。OSの利用条件の範囲で描画し、フォントファイルを顧客へ納品しない。顧客指定フォントは動画への商用描画を許す条件を確認し、版・配布元・条件を案件別に残す。CIの英語表示試験はDejaVu Sansを利用するが、日本語表示の合格確認には代用しない。

依存のライセンス宣言はCargoのキャッシュ内の各固定版manifestで確認できる。モデル・外部実行ファイル・フォントを含めたアプリ自体の販売や再配布は、今回の制作サービスと別の作業として扱う。今回の納品物は音声・動画・字幕であり、ソフトウェアやモデルの複製ではない。

照合用Cargo.lock SHA-256: `5874490003c9e44467b2ad8a8d131a7c3bdf23f32182cf333ddc945944e68d0e`
制作PCのYuGothM.ttc SHA-256: `cb2021bbfe40b21ec564848b8d702125661594e1b4f1eb2e77fd8a98b485cf19`
