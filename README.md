# KaraokeMaker

出品前にADJで実曲の使い勝手を確認します。書き出し試験の成功だけで、歌声の分離品質や歌いやすさを合格とは扱いません。[社内検証の手順](docs/adj-validation.md)に結果を残します。

音楽・動画を読み込み、このPCで個人用カラオケの音源と映像を作ります。

2026-10-04の追加構想を[音楽制作プラットフォームへの拡張計画](docs/music-platform-roadmap.md)に整理しました。作詞・曲の編集・MV・SNS投稿・公開後の分析をひとつの作品に紐付ける計画です。現在使える機能と未実装の機能を同書で区別しています。

## できること

- MP3・WAV・FLAC・M4A・MP4などを画面から読み込む。
- AIで歌声と伴奏を分け、伴奏を48kHz・24bitのWAVと320kbpsのMP3で保存する。高音質のMel-Band RoFormerと比較用のDemucsを選べます。
- 元の動画または背景を使い、HD・フルHD・1440p・横長4K（3840×2160）・縦長4K（2160×3840）のMP4を作る。画面の「出力解像度」で選べます。4Kは処理時間と容量が増えます。原本の細部は拡大しても増えません。
- キーを半音単位で-6〜+6、速度を0.5〜1.5倍にする。
- 任意の時刻付き歌詞を貼り付け、表示中の行を色で追う。速度を変えると映像・歌詞の時刻も合わせる。
- 完成した曲の画面から歌詞・時刻を直し、音源分離なしで背景付きの動画を作り直す。前の結果も残る。
- 作った結果を次回の起動後も開く。処理の取り消しやアプリ内の結果の削除もできる。

対象は1曲15分・1GBまで。曲によって歌声やコーラスが残り、楽器の音にも影響が出ます。正本の歌詞・LRC・単語の時刻を使って字幕を作れます。歌詞がない場合の聞き取りは確認用の候補です。メタルの叫び声など、時刻を確定できない場合は正本を残し、修正が必要だと表示します。

## Windowsで使う

1. リリースのZIPを展開する。
2. `KaraokeMaker.exe` を開く。ブラウザーにアプリの画面が出る。
3. 音楽・動画を選び、「カラオケを作る」を押す。
4. 完成した画面でMP4・MP3・WAVを保存する。
5. 使い終えたら、曲を選ぶ画面の「アプリを終了する」を押す。

FFmpegとffprobeが必要です。所有者のPCでは、すでに導入されたFFmpeg 8.0.1を使います。別のPCでは `winget install --id Gyan.FFmpeg -e` で導入してから開いてください。標準の高音質分離はNVIDIA GPU、更新されたドライバーとCUDA実行部品が必要です。このPCのRTX 5090で確認済みです。比較用DemucsはVulkanを使います。

カラオケ変換に使う音楽・動画・歌詞・生成物はこのPCで扱います。初回だけ、公開された標準分離モデル（約474MB）をHugging Faceから取得し、SHA-256を照合します。以後は取得したモデルを使います。有料APIやAPIキーは使いません。

## Sunoで音楽を作る

2026-10-04、Suno公式画面で更新後の契約による1回の生成と2候補の再生を確認しました。Admin Consoleからの取り込み・選択・再読込の実操作は未検証です。

Admin Consoleの「音楽制作 · Suno」、またはこのアプリの `/music` から開きます。歌詞・曲の雰囲気・担当キャラ・カラオケ版を公開するまでの時間を下書きとして保存し、生成する曲を選べます。

「Sunoで曲を生成する」を押すと、保存した下書きの操作要求を既存のAdmin Consoleの担当待ち列へ渡します。担当は対応するブラウザーツールでログイン済みの通常Chromeを操作し、Suno公式画面から一度だけ生成します。Sunoの契約の生成枠を使います。RustからSunoの非公式APIを呼びません。

通常Chromeの既存のログインをそのまま使います。アプリが専用プロフィールを作ったり、認証ファイル、Cookie、セッション値を取得したりしません。既存のAdmin Consoleと、通常Chromeを操作できるOAuthの担当環境が必要です。単体アプリだけでは自動生成の担当を起動できません。

Admin Consoleは、要求番号で担当の受付を照合する `/api/agent/todo/ensure` に対応した版が必要です。受付の応答を受け取れない場合も、保存した同じ要求の「受付を確認する」で担当番号を復旧します。未対応のConsoleには新しいToDoを追加せず、接続の確認を求めます。

担当の取得を曲の記録へ保存してから公式Createを操作します。担当の操作中にアプリが終了した場合や、結果を確認できない場合は再送せず、公式履歴から結果だけを回収します。担当待ちの要求は取り消せます。受付確認を繰り返す場合も同じ要求番号を使い、新しい生成要求を作りません。本人確認、契約や権限の操作が必要なら、その項目だけを止めます。

生成済みの曲は公式URLの実曲IDを下書きへ関連付けます。ファイルはSunoの正規画面から保存し、候補ごとの取り込みフォームでMP3を選びます。16MB・15分以内のMP3を検査し、原本と同じバイトのコピーを `data/music/audio/<下書きID>/<曲ID>/` に保存します。取り込んだ音源の試聴、採用した候補、下書きは再起動後も保持します。未取り込みの候補は採用できません。Gemini・Claudeでの作詞、JIZURAへの素材の自動受け渡し、キャラの歌声の指定、X・YouTube・TikTok・Instagramへの投稿・予約は今後の段階です。担当キャラと公開までの時間は、現在は保存する項目です。

Admin Consoleへの組み込みは `integrations/admin-console/music_bridge.rs` で行います。既存のConsoleにはこのモジュールと `/music` のルート、メニューだけを追加します。PCに導入する実行ファイルは `D:/CC/Apps/KaraokeMaker/KaraokeMaker.exe` で、必要になったときに窓を出さず起動します。単体のZIPは別のフォルダにも展開できます。

アプリは `127.0.0.1` にだけ接続を待ち受けます。通常は19531番のポートを使い、他のアプリが使用中なら空いているポートを選びます。元のファイルを変更せず、実行ファイルの隣の `data/jobs/<処理のID>/` に結果を作ります。アップロードした原本のコピーは、処理の終了時に削除します。

## 歌詞の貼り付け方

時刻は元の曲に合わせ、LRC形式で入力します。

```text
[00:12.34]最初の歌詞
[00:18.00]次の歌詞
```

`[offset:500]` のようなミリ秒の調整、1行の複数時刻、日本語に対応します。行全体を表示時間に沿って色で追うため、音節ごとの正確な表示にはなりません。動画に既存の歌詞が入っている場合は、歌詞の入力を省略できます。

## 開発・コマンドで使う

Rust 1.89以上とFFmpegが必要です。画面と処理をRustで実装し、HTMLのフォームを使います。PythonやNode.jsは必要ありません。

```powershell
cargo build --locked --release
./target/release/karaoke-maker.exe
./target/release/karaoke-maker-cli.exe check
./target/release/karaoke-maker-cli.exe --data ./data fetch-model
./target/release/karaoke-maker-cli.exe --data ./data convert ./song.mp4 --output ./results --title "曲名" --key -2 --tempo 0.8 --lyrics ./song.lrc
./target/release/karaoke-maker-cli.exe --data ./data serve --port 19531 --no-open
```

`--output` の中に処理ごとのフォルダを作り、既存の結果を上書きしません。映像を背景にする場合は `convert` に `--background` を付けます。`karaoke-maker-cli` はコマンドの結果を標準出力に表示し、デスクトップ用の実行ファイルはコンソールの窓を出しません。

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

テストでは歌詞の時刻・不正な入力・出力ファイルへのアクセス・動画の部分読み込みを確認します。FFmpegで実際に音源と字幕入り動画を作り、キー変更後の周波数、速度変更後の長さ、原本を変更していないことも確認します。CIではLinuxでテストし、WindowsでGPUを使う実行ファイルをビルドします。GPUでの音源分離は、GPUのないCIとは別に実機で確認します。

## 構成と依存先

- `src/web.rs`: 読み込み、進捗、再生、保存、結果の管理。
- `src/media.rs`: FFmpegによる抽出、キー・速度の変更、映像の出力。
- `src/model.rs`: モデルの取得・検証とGPUでの音源分離。
- `src/lyrics.rs`: LRCを読み込み、映像に表示する字幕を作る。
- `vendor/demucs-core`: [demucs-rs](https://github.com/nikhilunni/demucs-rs)のRust製HTDemucs実装。Apache-2.0のライセンスと固定した版を同梱。

標準の分離はKimのMel-Band RoFormerをRustとONNX Runtime/CUDAで実行します。モデル・実行部品の固定版と利用条件は[依存先一覧](docs/dependencies.md)に記録します。Demucsは比較用として保持します。コードのライセンスとモデルの利用条件を分けて扱い、全モデルのSHA-256を照合します。

FFmpegはPCに導入したものを呼び出し、このリポジトリやZIPには同梱しません。テスト用に確認した曲はHoliznaCC0の「There Ain't Nothin'」（CC0）です。曲や分離結果はGitHubに含めません。

このリポジトリは非公開です。自作曲の歌詞動画・カラオケ制作の受注に向けた[調査メモ](docs/service-spec.md)、[制作手順と残る不足](docs/production-workflow.md)、[依存先の版とライセンス](docs/dependencies.md)を保存しています。2026-10-04のオーナー指示で、先にADJ内で実曲の有用性を確かめます。出品と出品の相談は止めています。

制作者向けCLIでは `convert` と `render` に `--preset 1080p`、`--preset 1440p`、`--preset 4k`（3840×2160）、`--preset vertical`、`--style ./style.json` を付けられます。`render` は指定した完成音源や提供伴奏へ字幕を付け、音源分離なしで映像を作り直します。LRC・SRT・ASSも出力します。詳しい使い方は制作手順を参照してください。
