# 制作手順とアプリの不足

本書の品質基準は納品時の合格条件であり、現行の自動処理が商用品質に合格した実績ではない。

## 案件フォルダと入力

受領時に `orders/<匿名の案件ID>/` を作り、`source/` に歌入り音源・提供伴奏・歌詞・許可済み画像を置く。`rights/` に許可範囲と合意の控え、`edit/` に歌詞と時刻・配色設定、`renders/` に初稿と修正版、`delivery/` に検品済みの納品物を分ける。顧客の実名・曲・歌詞・画像をソース管理に入れない。原本を編集しない。アプリへ渡すものは原本のコピーとする。

1. [受注確認票](service-spec.md)を埋める。元の伴奏があれば、分離して作る伴奏より優先する。ffprobeで長さ・音声・映像の形式を確認し、受領ファイルのSHA-256を残す。
2. 伴奏がなければKaraokeMakerで一度分離する。サビ・強い歌声・残響のある箇所を含め15〜30秒を比較試聴する。許可済みの素材で可否を確かめ、購入前に顧客と伴奏品質を合意する。不合格なら元の伴奏を依頼するか受け付けない。
3. `lyric-subtitle-mv-workflow` に従い、顧客の歌詞を正本として一行ずつLRCに時刻を付ける。必要なASRは `whisper-cpp-japanese-asr` のRust/ネイティブ処理を候補時刻の取得だけに用いる。Python・外部AI・APIキーは使わない。
4. 空の時刻行を歌い終わりに置き、間奏で歌詞を消す。横は1〜2行、縦は短く改行し、顔・ロゴ・配信画面の表示と重ねない。字幕の色変化は行単位で、音節同期ではない。
5. 顧客ごとの配色、フォント、タイトル、字幕の改行と位置を決めて保存する。素材は自作または許可済みの画像・動画に限る。AI生成イラストは持ち込まれても使用しない。静止画を使う場合は `ffmpeg-safe-editing` の手順で許可済み音源付き背景動画にしてから、原映像として渡す。現在のアプリには別画像を配置する専用UIはない。
6. 承認済み伴奏と原曲をそれぞれ `render` へ渡す。字幕修正は編集LRCから再出力し、モデルでの分離を繰り返さない。
7. 出力を全編検品し、通常メッセージで初稿を送る。修正1回を反映し、再検品して正式納品する。納品時にファイル名・形式・使い方・保存期限をトークルーム本文に書く。

## CLIでの制作例

以下は許可済みの架空ファイル名。手元の案件フォルダで実行する。`convert` は分離を実行し、`render` は渡した音源をそのまま使って字幕を合成する。

```powershell
karaoke-maker-cli --data ./work-data convert ./source/song.wav --output ./renders/separation --title "曲名" --lyrics ./edit/song.lrc --preset full-hd --background
karaoke-maker-cli --data ./work-data render ./source/song.wav --output ./renders/lyric-video --title "曲名" --lyrics ./edit/song.lrc --preset 1080p --style ./edit/style.json --background
karaoke-maker-cli --data ./work-data render ./edit/approved-instrumental.wav --output ./renders/karaoke --title "曲名" --lyrics ./edit/song.lrc --preset full-hd --style ./edit/style.json --background
karaoke-maker-cli --data ./work-data render ./source/song.wav --output ./renders/vertical --title "曲名" --lyrics ./edit/song-vertical.lrc --preset vertical --style ./edit/style.json --background
karaoke-maker-cli --data ./work-data render ./source/song.wav --output ./renders/lyric-4k --title "曲名" --lyrics ./edit/song.lrc --preset 4k --style ./edit/style.json --background
karaoke-maker-cli --data ./work-data render ./edit/approved-instrumental.wav --output ./renders/karaoke-4k --title "曲名" --lyrics ./edit/song.lrc --preset 4k --style ./edit/style.json --background
```

各実行は新しいIDのフォルダを作り、場所を表示する。`render` はGPUモデル不要。歌入り動画もアプリ内の出力名は `karaoke.mp4` なので、納品コピーは `song-lyric.mp4`、伴奏版は `song-karaoke.mp4` に名前を付ける。元音源の名前を変えない。採用した出力フォルダと `settings.json` を案件に記録する。

`style.json` は次の形式。RGBは `#` を付けず6桁、フォントはPCにインストール済みで利用条件を確認したものを指定する。

```json
{"background":"301020","accent":"f0b4d8","font":"Yu Gothic"}
```

背景が元動画でも、余白の色と字幕の強調色には配色設定を使う。顧客ごとに同じ色を強制しない。初期の簡素な商品範囲は色・文字・タイトル・改行の調整で、完全に独自のアニメーションを約束しない。

```text
[00:12.30]最初の歌詞
[00:16.00]
[00:24.00]次の歌詞
[00:27.80]
```

この例は16〜24秒に歌詞を出さない。LRCは開始のみの形式なので、空行を終わりとして扱う。SRT/ASSにはその終了時刻を出力する。歌詞の正本とLRCを別に残し、勝手に文言を変えない。

字幕の装飾命令になる `\\`、`{}`、`<>` はSRT/ASSの表示用コピーで全角へ置き換える。歌詞の正本とLRCには元の文字を保持する。意図した装飾はフォント・配色設定で指定し、歌詞本文から命令を実行させない。

## 検品の合格条件

| 対象 | 合格条件と確認方法 |
| --- | --- |
| 伴奏 | 通常の試聴音量で主旋律の歌詞が明瞭に残らず、伴奏に大きな欠落・揺れ・金属音・クリックがない。全編をヘッドホンとスピーカーで確認。コーラスの有無は顧客と合意 |
| 原曲 | 歌入り版は承認された完成音源を使う。キー0・速度1。曲構成・歌詞を勝手に変えず、前後を切らない |
| 字幕 | 正本と全行照合して誤字なし。目標の開始誤差は±100ms以内、歌い終わり・間奏は顧客の意図と一致。ASR結果だけで合格にしない |
| 表示 | 全編を目視し、冒頭・サビ・終盤のフレームも確認。日本語の欠け、画面外の文字、読めない長文、顔との重なりなし。縦は左右と上下の配信UIの領域を避ける |
| 音量 | クリップ・途中の無音・不自然な音量跳びなし。アプリはWAVのtrue peakを測り、必要なら単純な減衰で約-3.1dBTPへ下げる。MP3/AACは0dB未満を確認する。商用納品の-1dBTP以下は別途確認 |
| 形式 | ffprobeで横1920×1080または縦1080×1920、30fps、H.264/yuv420p、AAC、WAV24bit/44.1kHz/stereo、MP3を確認。動画・WAVと元曲の長さの差は±0.25秒未満 |
| 再制作 | 原本のSHA-256が受領時と同じ。LRC、SRT、ASS、配色、採用伴奏と設定を保存し、修正版が別フォルダにある |
| 納品 | 新しいプレイヤーで再生・頭出しできる。正しい案件のファイルだけを添付。権利・用途・クレジット・実績掲載許可を照合 |

音量測定はインストール済みFFmpegの `ebur128=peak=true` 等で行う。ピーク不合格は単純な音量低下で調整し再測定し、音質に関わるマスタリングを無断でしない。自動で書かれた `ffmpeg.log` と、手作業の検品日時・結果をPC内に保存する。未検査の項目があれば納品しない。

## 今回実装した小さな不足

- CLIの `--preset hd/1080p/1440p/4k/vertical` で720p・横1080p・横1440p・横4K・縦1080pを選べる。`full-hd` も1080pの別名として使える。30fpsを固定し、字幕のキャンバス・文字サイズ・輪郭・余白を合わせる。映像を拡大した後でlibassが文字を直接描画する。既存画面と古い保存設定は720pを保持する。
- `--style` で背景色・字幕の強調色・フォントを案件別に保存できる。
- `render` で完成音源や提供伴奏を字幕付き動画にし、音源分離なしで修正版を作れる。
- LRCを原曲時刻で、SRT/ASSを出力時刻で保存できる。空のLRC行で間奏の表示を終えられる。

新しいRust依存は追加していない。画面から解像度を選べ、完成後はLRCと時刻を修正して音源分離なしで背景付き動画を作り直せる。波形や各行のドラッグ編集・自動同期は今後の項目。

1440p/4K指定時の形式検品では上の1080p解像度の条件を、それぞれ2560×1440/3840×2160へ置き換える。品質と容量に応じた設定・書き出しの測定結果は [4k-validation.md](4k-validation.md) を参照する。歌入り・伴奏の両方で指定解像度を確認する。

## 残る不足と暫定運用

| 不足 | 今の対応 | Issue |
| --- | --- | --- |
| 曲ごとの分離品質を判定した商用実績 | 購入前の比較試聴。提供伴奏優先、不合格は受注しない | [音源分離の評価](https://github.com/AutoDevJapan/KaraokeMaker/issues/2) |
| 開始・終了を波形を見て直す編集画面 | 完成画面のLRC修正と再出力を使用し全編確認 | [歌詞編集画面](https://github.com/AutoDevJapan/KaraokeMaker/issues/3) |
| 背景画像・ロゴ・配置を保存するテンプレート | 色・フォント・改行を案件別に指定。許可済み背景はFFmpegで準備 | [動画テンプレート](https://github.com/AutoDevJapan/KaraokeMaker/issues/4) |
| 48kHz・納品検査の自動記録 | 44.1kHz標準、ピーク測定は実装済み。試聴・目視と商用基準は手動確認 | [納品の形式と検品](https://github.com/AutoDevJapan/KaraokeMaker/issues/5) |

2026-10-04のオーナー指示により、現在はADJでの本人の試用を先に行い、出品と出品の相談を止める。将来の受注開始には、権利確認済みサンプルでの全編試聴・字幕目視と制作時間の記録が必要。元の伴奏を提供してもらう案件でも、表示と納品の検品は省略しない。

利用する版とライセンスは [dependencies.md](dependencies.md) を参照する。
