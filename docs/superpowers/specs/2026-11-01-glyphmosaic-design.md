# glyphmosaic 設計書

日付: 2026-11-01
状態: 承認待ち（実装前）

## 1. 何を作るか

任意の動画を読み込み、フレームごとに `cols × rows` のグリッドへ分割し、各セルを
**1つの可視 Unicode 文字のグリフ**で置き換えた動画を生成する Rust 製 CLI。

参照動画と同じ「4×3 = 12文字/フレーム」「左にモザイク、右に元映像と12個の
コードポイント」というレイアウトも生成できる。フォント辞書の構築・照合・
書き出しをすべてコマンドで再実行でき、第三者が同じ手順を踏めるようにする。

想定コマンド:

```powershell
glyphmosaic dict build --fonts "fonts/*.ttf" --out fonts.gmd
glyphmosaic render -i badapple.mp4 -d fonts.gmd -o badapple_4x3.mp4 `
  --grid 4x3 --layout panel --cell-height 360
glyphmosaic report badapple_4x3.codepoints.tsv
```

## 2. なぜ / 成功条件

- 参照動画の手法（全グリフを描画 → セルいっぱいに引き伸ばし → MSE 最小）を、
  第三者が同じ手順で再実行できること。
- 出力が音声付きで再生できる MP4 で、各セルが「実際に見える」1文字であること。
- 同じ入力・フォント・設定なら、同じ文字列列になること（決定的）。
- 参照動画相当（6,955フレーム、4×3、1080p）を現実的な時間で処理できること。
  目標: 照合 60 秒以内、エンコード込みで 10 分以内（この PC）。
- 全セルのコードポイントと統計（使用文字数、1回だけ出た文字、最多使用）を
  ファイルに出力できること。

## 3. 非目標

- 参照動画と1文字も違わない完全一致。フォント版・ラスタライザ・前処理が
  公開されていないため、見た目と手法の再現を目標にする。
- フォントや元動画の同梱（ライセンス・著作権のため）。
- リアルタイム再生、GUI、色絵文字、複雑な文字 shaping（合字・結合）。
- AI による画像生成。選ぶのは実フォントのグリフのみ。

## 4. 手法（参照動画との対応）

### 4.1 辞書構築 `dict build`

1. 各フォントの cmap を列挙し、符号化されている文字を集める。
2. 空白・制御文字・既定で不可視の文字・異体字セレクタを除外する。
   Private Use Area は既定で除外（`--include-pua` で含められる）。
3. 1文字ずつ高解像度（128px em）でラスタライズする。
4. **インクのバウンディングボックスでクロップ**し、`64×64` に非等方リサイズする。
   これが「グリフをセルいっぱいに引き伸ばす」処理。`.` のような小さい記号も
   大きな白い塊になる（参照動画の FAQ と同じ挙動）。
5. `16×16`（full）と `4×4`（coarse）の u8 特徴量を作る。
6. エントリは `(codepoint, font_index, full[256], coarse[16])`。
   フォントは複数指定でき、同じ文字でもフォントが違えば別エントリ。

辞書は独自バイナリ `.gmd`（mmap 可能）。ヘッダにフォーマット版、作成パラメータ、
フォントのパス・SHA-256・名前を保存する。

### 4.2 入力とセル特徴 `render`

v1 の入力範囲:

- SDR / 8bit / プログレッシブ。HDR・10bit・インターレースは明確なエラーで止める。
- 回転メタデータは ffmpeg の autorotate に任せる。SAR は正方形へ正規化する。
- VFR は ffprobe の平均 fps で CFR 化して出力する（音声は元ファイルから mux）。
- 音声は先頭トラックのみ。`--audio copy` で MP4 に入らないコーデックなら
  エラーにし、`--audio aac` を案内する。

1. ffmpeg で RGB24 の生フレームをデコードする（音声は別入力として保持）。
2. 輝度へ変換し、`cols×16 × rows×16` へボックス縮小する。
   各セルがちょうど `16×16` になる。セルが正方形でない場合は、セル画像を
   正方形特徴空間へ正規化する（参照動画のセルは 4:3 動画の 4×3 で正方形）。
3. 各セルの `16×16` から `4×4` の coarse 特徴を作る。

### 4.3 照合

- coarse（16次元）で全辞書をスキャンし、MSE 上位 K 件（既定 256）を残す。
- 上位 K 件だけ full（256次元）で再スコアし、最小のエントリを選ぶ。
- スコアは整数 MSE。同点は辞書インデックス順で決定的に解決する。
- `--exact` を付けると coarse を省き、full で全辞書を走査する（参照動画の
  「全グリフ総当たり」に最も近い。遅い）。
- 同一のセル特徴（黒セル・白セルなど）はハッシュでまとめ、結果を再利用する。
- rayon でセル並列。AVX2 を実行時検出し、なければスカラーにフォールバック。

### 4.4 描画

- `--layout mosaic`: モザイクのみ。セル高さ `--cell-height`、セル幅は元動画の
  セル縦横比から算出。
- `--layout panel`: 左にモザイク、右に元フレーム（縮小）と 12 個の
  `U+XXXX` 一覧。参照動画と同じ構成。
- 選ばれたグリフはフォントから出力セルサイズで再ラスタライズし、LRU キャッシュ
  （既定 512MB）に保持する。辞書構築時と同じフォントが必要。
- コードポイント表示は内蔵の 5×7 ビットマップフォントで描く（システムフォント
  非依存）。

### 4.5 エンコード

- RGB24 の生フレームを ffmpeg の stdin へストリーミングし、H.264 (libx264,
  crf 18) で書き出す。音声は既定で AAC に再エンコード、`--audio copy` で
  コピーも選べる。
- 進捗は `indicatif` で表示。`--limit N` で先頭 N フレームだけ試せる。

## 5. CLI

```
glyphmosaic dict build --fonts <glob>... --out <file.gmd>
    [--include-pua] [--min-ink 1] [--jobs N]

glyphmosaic render -i <video> -d <file.gmd> -o <out.mp4>
    [--grid 4x3] [--layout mosaic|panel] [--cell-height 240]
    [--panel-width 480] [--top-k 256] [--exact]
    [--fonts <glob>...] [--font-root <dir>]
    [--dump-codepoints <file.tsv>] [--stats <file.json>]
    [--audio aac|copy|none] [--crf 18] [--limit N] [--threads N]

glyphmosaic inspect <file.gmd>
glyphmosaic report <codepoints.tsv>
```

`report` は参照動画の Stats と同じ項目を出す:
総ピック数、異なり文字数、1回だけ出た文字数、最多使用トップ10。

## 6. 辞書フォーマット

```
magic "GMD1" | version u32 | header_len u32 | entry_count u32
header (JSON): cell_feature=16, coarse_feature=4, fonts[{path,sha256,name}],
               unicode_version, created_by, params
entries (fixed size, little endian):
  codepoint u32 | font u16 | reserved u16 | full [u8;256] | coarse [u8;16]
```

`memmap2` で読み込み、検索時は coarse 配列だけを連続アクセスする。

フォントの解決順: `--fonts` で渡されたファイル → `--font-root` 配下の同名ファイル
→ ヘッダのパス。いずれも SHA-256 を照合し、見つからない/不一致ならエラー。
辞書はフォントそのものを含まないため、別マシンで使う場合は同じフォントを
`--fonts` で渡す。

## 7. 性能設計

- 参照動画の 410,510 エントリは比較用の上限値。本実装の既定フォントセットは
  約26万エントリ（Jigmo 146k + Unifont 57k + Noto 系 60k 弱）を想定する。
  性能目標はこの26万で測り、410k の合成辞書でも bench を回す。
- 410k エントリ × coarse 16B = 6.6MB。L3 (12MB) に収まり、全走査が
  メモリ帯域で破綻しない。
- 1セルあたり 410k 件 × 16次元の二乗誤差。AVX2 で 16 バイトずつ処理。
- セル特徴の重複排除で、黒/白が多い Bad Apple ではクエリ数が大きく減る。
- フレームは1枚ずつ流し、動画全体をメモリに持たない。
- 目標（この PC: Core Ultra 7 258V）:
  - 辞書構築（約26万グリフ）: 2分以内
  - 照合（6,955フレーム × 12セル、重複排除あり）: 60秒以内
  - 1080p 書き出し込み: 10分以内
- 測定用に `--limit 500 --no-encode`（または `bench` サブコマンド）を用意する。
- GPU (wgpu) は v1 では実装しない。照合器を trait で切り出し、後から差し替え
  可能にする。

## 8. 再現性

- 辞書ヘッダにフォントの SHA-256、Unicode 版、全パラメータを記録。
- 照合は整数演算で、スレッド数に依存しない。タイブレークは辞書順。
- 保証するのは同一マシン・同一ビルド・同一フォントでの決定性。フォントの
  ラスタライズと縮小は浮動小数なので、マシン/CPU が変わると稀に1文字差が
  出る可能性がある（README に明記）。
- `--dump-codepoints` は `frame, cell, U+XXXX, char, font, mse` の TSV。
- `--stats` は JSON。異なり文字数はコードポイント単位で数える（フォント違いの
  同じ文字は1つ）。README に参照動画の統計との比較方法を書く。

## 9. リポジトリ構成

```
glyphmosaic/
  Cargo.toml
  README.md
  LICENSE-MIT / LICENSE-APACHE
  .gitignore
  docs/superpowers/specs/2026-11-01-glyphmosaic-design.md
  scripts/fetch-fonts.ps1
  src/
    main.rs, cli.rs, error.rs
    dict/{mod.rs, build.rs, format.rs}
    raster.rs
    matching/{mod.rs, scan.rs, simd.rs}
    video/{probe.rs, decode.rs, encode.rs}
    render/{mod.rs, glyph_cache.rs, text.rs, panel.rs}
    stats.rs
  tests/integration.rs
  benches/matching.rs
```

主要クレート: `clap`, `anyhow`, `thiserror`, `serde`/`serde_json`, `ttf-parser`,
`fontdue`, `rayon`, `memmap2`, `indicatif`, `sha2`, `unicode-general-category`,
`fast_image_resize`, `bytemuck`。

## 10. フォント

`scripts/fetch-fonts.ps1` で以下を `fonts/` に取得し、SHA-256 を検証する
（リポジトリにはコミットしない）。参照に近づける既定セットは4系統・9ファイル:

| 系統 | ファイル | 入手元 | ライセンス |
|---|---|---|---|
| Unifont | `unifont-17.0.05.otf` | unifoundry.com（版固定） | OFL 1.1 / GPLv2+ embedding exception |
| Jigmo | `Jigmo.ttf`, `Jigmo2.ttf`, `Jigmo3.ttf` | Jigmo-20250912.zip（版固定） | CC0 1.0 |
| Noto Sans | `NotoSans-VF.ttf`, `NotoSansSymbols2-Regular.ttf`, `NotoSansMath-Regular.ttf` | google/fonts の固定コミット | OFL 1.1 |
| Noto CJK JP | `NotoSansJP-VF.ttf`, `NotoSerifJP-VF.ttf` | google/fonts の固定コミット | OFL 1.1 |

- 参照動画は Unifont 17.0.05 と Jigmo を使用しているため、その2つは版を固定。
- 合計エントリは約26万。参照動画の 155,230文字 / 410,510グリフとはフォント
  構成が違うため、統計値は一致しない（同じ手法・同じ見た目を目標にする）。
- フォントの組み合わせは `--fonts` で自由に変えられる。`--fonts` を省略した
  場合は `fonts/` を再帰的に読む。
- 既存のインストール済みフォント（Noto Sans JP など）も `--fonts` で使えるが、
  再現性のため README では取得スクリプト版を推奨する。

## 11. テスト

- unit:
  - インク bbox のクロップと引き伸ばし
  - 16×16 → 4×4 の coarse 生成
  - 照合結果が総当たり（小さな辞書）と一致
  - 辞書の書き出し/読み込みラウンドトリップ
  - U+XXXX のパース、統計集計
- integration:
  - ffmpeg で生成した合成クリップ（2秒）を最後まで通す
  - `--limit`、音声なし、`--layout panel` のスモーク
  - フォントは環境変数 `GLYPHMOSAIC_TEST_FONT` がある場合のみ実行
- bench:
  - 410k エントリの合成辞書で 1クエリあたりの時間を測定

## 12. リスクと対策

- **可変フォント**: `fontdue` は variation axis を扱えないため、既定インスタンス
  で描画する。Noto Sans JP VF は Regular 相当になる。必要なら後で `swash` に
  差し替える。
- **近似探索**: coarse→full の2段階は total 総当たりとわずかに差が出る。
  `--exact` を用意し、サンプルでの一致率を README に載せる。
- **メモリ**: グリフキャッシュは上限付き LRU。超過時は再ラスタライズ。
- **Windows のパイプ**: ffmpeg の stdin/stdout を binary で扱い、デッドロック
  しないよう読み書きを別スレッドにする。
- **フォントの再配布**: 取得スクリプトのみ。ビルド済み辞書にフォントは含めない。
- **参照動画の入手**: デモ用にローカルで yt-dlp を使う場合は保持せず、成果物に
  同梱しない。テストは合成クリップで行う。

## 13. 私の判断（変更可能）

- 名前は `glyphmosaic`、CLI も同じ。
- v1 は CPU のみ。GPU は後回し。
- 既定は coarse→full の高速モード。`--exact` で忠実モード。
- 入力は SDR / 8bit / プログレッシブ / 先頭音声のみ。HDR・10bit・インター
  レースはエラー。
- 参照レイアウトは 1920×1080（モザイク 1440×1080 + パネル 480×1080）。
- 音声の既定は AAC 再エンコード。
- フォント取得はスクリプト + SHA-256 固定（4系統9ファイル）。
- 決定性は同一マシン・同一ビルドまで保証。マシン間はベストエフォート。
- 辞書はフォントを含まない。別マシンでは `--fonts` で同じフォントを渡す。
- Rust と Git が未導入なので、winget で導入する（VS Build Tools は既存）。

## 14. 承認

この内容で進めてよいか。承認後に実装計画（writing-plans）を作り、その順で
実装する。デモは合成クリップで検証し、参照動画での実行はユーザーが用意した
ファイルに対して行う。
