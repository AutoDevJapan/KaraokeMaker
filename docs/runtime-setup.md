# 高音質分離の初回準備

高音質分離には対応するNVIDIA GPUとドライバー、FFmpeg/ffprobeが必要です。CUDA Toolkit全体の導入や環境変数の手動設定は必要ありません。

通常はZIPを展開し、`KaraokeMaker.exe` で音源と解像度を選んで作成します。固定版のモデルを取得し、実行部品がなければMicrosoft/NVIDIAの公式配布元から固定版を取得します。音声や歌詞を送信する処理ではありません。モデル・部品のサイズとSHA-256が一致してから使用します。

実行部品はアプリの保存先の `data/runtime/onnxruntime-cuda-1.24.4` に置き、配布元のライセンス・通知原文と、版・配布URL・ファイルのハッシュを記録した `runtime-manifest.json` を保持します。`--data` を指定した場合は、その保存先の中へ導入します。先に準備する場合は次を実行します。

```powershell
./karaoke-maker-cli.exe setup-runtime
./karaoke-maker-cli.exe --data ./my-data setup-runtime
```

同じアプリの実行ファイルの隣や `runtime` サブフォルダに必要な部品がそろっている場合は再利用できます。以前の制作PC向け導入先も互換性のため参照しますが、未導入のPCでその場所を作る必要はありません。`setup-runtime` はこれらの場所に依存せず、選んだアプリの保存先へ導入します。

準備中は既存の進捗欄に状況を表示します。中断・失敗した未完了の展開先を使用せず、照合済みの取得アーカイブは再試行で再利用します。導入済みの部品が壊れていた場合は元の内容を別名で保持して導入し直します。導入完了後のアーカイブ削除に失敗した場合は、導入先の `setup-cleanup-warning.txt` と標準エラーへ対象・理由を残します。

固定値は `resources/cuda-runtime.json` に記録します。ONNX Runtimeは[NuGetの固定版](https://www.nuget.org/packages/Microsoft.ML.OnnxRuntime.Gpu.Windows/1.24.4)と公式カタログのSHA-512を照合して取得したアーカイブのSHA-256を固定しています。CUDA Runtime・cuBLAS・cuFFTは[NVIDIA CUDA 12.8.1の再配布メタデータ](https://developer.download.nvidia.com/compute/cuda/redist/redistrib_12.8.1.json)、cuDNNは[NVIDIA cuDNN 9.14.0の再配布メタデータ](https://developer.download.nvidia.com/compute/cudnn/redist/redistrib_9.14.0.json)のSHA-256に対応します。各部品の利用条件は同梱された原文を確認してください。

初回準備・起動の成功は、実曲の音質、字幕の±100ms同期、全編試聴・目視の合格を示しません。これらは実曲の受入記録で別に確認します。
