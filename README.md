# KaraokeMaker

音楽・動画を読み込み、このPCで個人用カラオケの音源と映像を作ります。

## できること

- MP3・WAV・FLAC・M4A・MP4などを画面から読み込む。
- AIで歌声と伴奏を分け、伴奏をWAV・MP3で保存する。
- 元の動画または背景を使い、1280×720のMP4を作る。
- キーを半音単位で-6〜+6、速度を0.5〜1.5倍にする。
- 任意の時刻付き歌詞を貼り付け、表示中の行を色で追う。速度を変えると映像・歌詞の時刻も合わせる。
- 作った結果を次回の起動後も開く。処理の取り消しやアプリ内の結果の削除もできる。

対象は1曲15分・1GBまで。曲によって歌声やコーラスが残り、楽器の音にも影響が出ます。歌詞の自動聞き取りや単語ごとの自動同期は、現在の版にはありません。

## Windowsで使う

1. リリースのZIPを展開する。
2. `KaraokeMaker.exe` を開く。ブラウザーにアプリの画面が出る。
3. 音楽・動画を選び、「カラオケを作る」を押す。
4. 完成した画面でMP4・MP3・WAVを保存する。
5. 使い終えたら、曲を選ぶ画面の「アプリを終了する」を押す。

FFmpegとffprobeが必要です。所有者のPCでは、すでに導入されたFFmpeg 8.0.1を使います。別のPCでは `winget install --id Gyan.FFmpeg -e` で導入してから開いてください。Vulkanに対応したGPUと、更新されたGPUドライバーが必要です。

カラオケ変換に使う音楽・動画・歌詞・生成物はこのPCで扱います。初回だけ、公開された分離モデル（約84MB）をHugging Faceから取得し、SHA-256を照合します。以後は取得したモデルを使います。有料APIやAPIキーは使いません。

## Sunoで音楽を作る

Admin Consoleの「音楽制作 · Suno」、またはこのアプリの `/music` から開きます。歌詞・曲の雰囲気・担当キャラ・カラオケ版を公開するまでの時間を下書きとして保存し、生成する曲を選べます。

「Sunoで曲を生成する」を押したときだけ、歌詞と曲の雰囲気をSunoへ送ります。Sunoの契約の生成枠を使います。音楽生成は、非公式のSunoエンドポイントを呼ぶRustの実装です。外部のAPI販売サービスや有料CAPTCHAサービスは使いません。

初回は「Sunoに接続」で専用のChrome画面を開き、本人がSunoへログインします。通常のChromeプロファイルや認証ファイルを読みません。ログインした専用画面のセッションを、このプログラムがSunoへの通信にだけ使います。認証値をログ・画面・プロジェクトファイルに残しません。Chrome自体が保存する専用のブラウザーデータは `data/music/suno-browser/` に置き、GitHubや配布ZIPに含めません。

Sunoが本人確認を要求するときは、自動生成を始めず、本人が接続画面を操作します。生成要求を送った後で通信が切れた場合は、自動で再送しません。Sunoの履歴を確認して重複生成を避けます。

曲候補の試聴・完成した候補の選択までを扱います。ファイルのダウンロードはSunoの正規画面から行います。Gemini・Claudeでの作詞、JIZURAへの素材の自動受け渡し、キャラの歌声の指定、YouTubeへの投稿・予約は今後の段階です。担当キャラと公開までの時間は、現在は保存する項目です。

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

分離モデルは [set-soft/audio_separation](https://huggingface.co/set-soft/audio_separation) の `Demucs/htdemucs.safetensors` を使います。元の[Demucs](https://github.com/facebookresearch/demucs)はMITライセンスです。モデルの期待するSHA-256はソースに固定し、変更されたファイルを自動では採用しません。

FFmpegはPCに導入したものを呼び出し、このリポジトリやZIPには同梱しません。テスト用に確認した曲はHoliznaCC0の「There Ain't Nothin'」（CC0）です。曲や分離結果はGitHubに含めません。

このリポジトリは非公開です。自作曲の歌詞動画・カラオケ制作の受注に向けた[サービス仕様](docs/service-spec.md)、[制作手順と残る不足](docs/production-workflow.md)、[依存先の版とライセンス](docs/dependencies.md)を保存しています。出品はオーナーの判断待ちです。

制作者向けCLIでは `convert` と `render` に `--preset 1080p`、`--preset 1440p`、`--preset 4k`（3840×2160）、`--preset vertical`、`--style ./style.json` を付けられます。`render` は指定した完成音源や提供伴奏へ字幕を付け、音源分離なしで映像を作り直します。LRC・SRT・ASSも出力します。詳しい使い方は制作手順を参照してください。
