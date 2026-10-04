# 使用する版とライセンス

確認日: 2026-10-04。今回の機能は既存依存だけを使い、新しいcrateを追加していない。Rustの依存はCargo.lockに固定し、ビルド・テストは --locked で行う。下表はcargo metadataから取得した直接依存（開発用を含む）の宣言で、ライセンス全文の代わりではない。

| 依存 | 固定した版 | crateのライセンス宣言 |
| --- | --- | --- |
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
| tokio-tungstenite | 0.29.0 | MIT |
| tokio-util | 0.7.19 | MIT |
| tower | 0.5.3 | MIT |
| uuid | 1.26.1 | Apache-2.0 OR MIT |

## 音源分離と外部プログラム

- demucs-core: 上流の固定commitは `5d9f61a2e15987c5ea9a9fe92300b64d994bcd12`、Apache-2.0。`vendor/demucs-core/UPSTREAM.md` と同梱LICENSEに記録する。
- HTDemucsのsafetensors: `set-soft/audio_separation` の `Demucs/htdemucs.safetensors`、84,030,696 bytes。期待SHA-256は `8193504cdfb3943adaf039b8acb524a46e87ebf232c383ac7a32c80a6578423e`。配布元モデルカードの宣言はMIT、上流DemucsもMIT。配布元の宣言と原モデルの出所を記録し、新しいモデルへ交換する際は条件を再確認する。[モデルカード](https://huggingface.co/set-soft/audio_separation/blob/main/README.md)、[上流LICENSE](https://github.com/facebookresearch/demucs/blob/main/LICENSE)。モデルをZIPへ同梱しない。
- 制作PCのFFmpeg/ffprobe: `8.0.1-full_build-www.gyan.dev`。確認したビルドは `--enable-gpl --enable-version3`。外部実行ファイルとして呼び出し、本リポジトリや販売動画にプログラムを同梱しない。各PCの実行版・設定は `ffmpeg -version` で制作記録に残す。LGPL/GPLの違いは構成に依存する。[FFmpeg公式ライセンス案内](https://ffmpeg.org/legal.html)
- libass、libx264、libmp3lame、Rubber Bandは上記FFmpegビルドの機能として利用する。単独のライブラリの再配布は行わない。別のFFmpegビルドで利用するときはASS、日本語フォント、rubberband、H.264/AAC/MP3エンコードの動作を確認する。
- 標準フォント: Windowsに導入済みのYu Gothic。OSの利用条件の範囲で描画し、フォントファイルを顧客へ納品しない。顧客指定フォントは動画への商用描画を許す条件を確認し、版・配布元・条件を案件別に残す。CIの英語表示試験はDejaVu Sansを利用するが、日本語表示の合格確認には代用しない。

依存のライセンス宣言はCargoのキャッシュ内の各固定版manifestで確認できる。モデル・外部実行ファイル・フォントを含めたアプリ自体の販売や再配布は、今回の制作サービスと別の作業として扱う。今回の納品物は音声・動画・字幕であり、ソフトウェアやモデルの複製ではない。

照合用Cargo.lock SHA-256: `337e21b1298dbf91453e7195c90a0a050486224d7fae6d426840da80cd8a84f3`
制作PCのYuGothM.ttc SHA-256: `cb2021bbfe40b21ec564848b8d702125661594e1b4f1eb2e77fd8a98b485cf19`
