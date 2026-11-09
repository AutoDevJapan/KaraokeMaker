# Windowsの配布物

ZIPを展開し、`KaraokeMaker.exe` を開きます。CLIは `karaoke-maker-cli.exe` です。
作成した曲・設定・取得モデルは実行ファイルの隣の `data` に保存します。
更新前にアプリを終了し、既存の `data` と原本を保持してください。

FFmpeg/ffprobe、GPUドライバー、CUDA実行部品、分離モデルは同梱しません。
導入条件とモデルの取得・ライセンスはREADMEとDEPENDENCIESを確認してください。
FFmpegは `winget install --id Gyan.FFmpeg -e` などで別途導入します。
高音質分離の初回利用では約474MBのモデルを取得しSHA-256を検証します。
第三者の部品とモデルには、それぞれの条件が適用されます。

`karaoke-maker-cli.exe --help` と `--version` は同梱CLIの起動確認に使えます。
CIでのこの確認は、GPU分離・ブラウザー操作・実曲品質の合格を示しません。
実曲の全編試聴・歌詞時刻・描画と、元音源が変わらないことを別に確認してください。
CI artifactは検証用成果です。一般向けリリース・販売開始とは区別します。
