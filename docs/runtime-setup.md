# 高音質分離の初回準備

高音質分離にはWindows x64、対応するNVIDIA GPUとドライバー、FFmpeg/ffprobe、最新の[Microsoft Visual C++ v14 再頒布可能パッケージ（x64）](https://aka.ms/vc14/vc_redist.x64.exe)が必要です。[ONNX Runtimeの導入条件](https://onnxruntime.ai/docs/install/#requirements)にも対応する実行用部品です。Visual StudioやCUDA Toolkit全体の導入、環境変数の手動設定は必要ありません。

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

## 字幕候補の初回準備

英語の正本歌詞からの時刻合わせと、歌詞の聞き取り候補にも固定版の部品を使います。完成音源の分離後、自動字幕が必要になった段階で、選んだ保存先の `models/subtitles` に取得します。時刻付きLRCを入力した場合や字幕を無効にした場合には取得しません。音声・歌詞・映像を取得先へ送信することはありません。

- 英語CTCは `models/subtitles/ctc-729c1a6` にモデル・語彙・原モデルと変換版の出所を保持します。
- Whisperは `models/subtitles/whisper-v1.9.2` にモデルとライセンス、`whisper-cpp-v1.9.2` にWindows x64の実行部品を保持します。

いずれも `resources/subtitle-assets.json` の固定URL・サイズ・SHA-256と一致してから使用します。実行部品のZIP内13ファイルもそれぞれ照合し、未完了の展開先は使用しません。導入済みの照合済みファイルは再取得せず使用でき、破損していたものは `.invalid-...` の別名で保持して準備し直します。初回は英語CTCが約95MB、Whisperモデルが約574MB、Whisper実行部品ZIPが約8MB必要です。LRCでの修正・再出力はこれらのモデルを再実行しません。

字幕部品の通信が停止している間も取り消しを確認します。大容量ファイル全体の取得期限は20分、接続や一度の応答待ちは30秒で、取り消された未完了ファイルは使用しません。

OSのDNS解決は通信の取り消し後も終了まで残る場合があります。字幕取得のruntimeはアプリ内で共有し、DNSなどのblocking処理は同時2件までにします。取り消した取得処理は保存ファイルやcache lockを解放し、残るDNSはホスト名の解決だけを行います。再試行で新しいruntimeや無制限のワーカーを作りません。アプリは変換処理の終了後にruntimeを停止し、DNSの終了待ちは100msで打ち切るため、その後もOS解決が残っていれば終了時までに処理できたと主張しません。この待ち時間はOSのスケジューリングや同期ファイル操作の厳密な応答保証ではありません。

字幕候補の準備が失敗した場合も、入力した正本の歌詞を確認用に残します。認識結果や補間を確定した歌詞・時刻として扱わず、試聴と正本で確認してください。字幕候補の自動準備はWindows向けで、古い制作PCの固定Dドライブは不要です。
