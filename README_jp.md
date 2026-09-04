# jals

[![CI](https://github.com/topi-banana/jals/actions/workflows/ci.yml/badge.svg)](https://github.com/topi-banana/jals/actions/workflows/ci.yml)

**lossless（無損失）な構文木**を基盤にした、Rust 製の Java ツールチェインです。

`jals` は Java のソースを完全忠実な CST（具象構文木）へとパースします。空白やコメントを含む
すべてのバイトが保持され、その木を土台にソースツールを構築します。現在はコードフォーマッタ・
linter・language server（LSP）を提供しており、いずれも名前解決・ファイル横断の型インデックス・
型推論/型検査を行う共通のセマンティック層（`jals-hir`）に支えられています。この層はプロジェクトの
コンパイル済み classpath や `[dependencies]`（明示的なローカル/リモート jar、および transitive な
`git`/`path` JALS source project。jar に source が無ければ逆コンパイルして読める Java を生成）から型を
解決することもできます。これらと並んで、`jals.toml` マニフェストから JDK の `javac` / `java` を
ラップし、コンパイル前に sandbox 化された Rhai build script も実行できる Cargo 風のビルド
フロントエンド（`jals build` / `run` / `test` / `clean` / `init`）も備えています。

> The English README is available at [README.md](README.md).

## 特長

- **無損失かつエラー耐性。** lexer は入力の全バイトをちょうど 1 トークンに対応させ、parser は
  不正な入力に対しても必ず木を返します。どちらも panic しません。
- **Java 26 文法に対応。** class / interface / enum / record、sealed 型、アノテーション、lambda、
  switch 式、パターン（record パターンや guard を含む）などをサポートします。
- **保証付きのフォーマッタ。** コメントを削除・並べ替えすることがなく、冪等
  （`format(format(x)) == format(x)`）で、意味のあるトークンの多重集合が変わるのは**宣言された操作**が
  それを許す箇所だけです。`Config::default()` が許すのは方言の末尾カンマ削除、rustfmt 既定 on の
  `remove-nested-parens` と `force-switch-arm`、そして import の sort（列の並べ替えであり多重集合は
  変わらない）です。この検査を通らなかった出力は破棄され、入力がそのまま返ります。拒否されたファイルは
  「変更不要だったファイル」とバイト単位で区別できないため、その事実は実行時に**報告されます**。
- **本物のセマンティクスを持つ linter。** 構文的なチェックにとどまらず、`jals lint` は名前解決と
  型推論を CST 上で行い、未使用のローカル変数・型不一致・報告されていない検査例外・到達しない
  条件分岐を検出します（単なるパターンマッチではありません）。
- **フレームワーク不要のテスト。** テストとは `#[test]` を付けたメソッドのことで、JUnit も
  annotation processor も launcher jar も要りません。`jals test` は各テストを専用の JVM で並列に
  実行し、`cargo nextest` と同じ形で結果を報告します。`jals build` はそれらを 1 つもコンパイル
  しません。
- **Cargo 風の Java ビルド。** `Cargo.toml` の Java 版にあたる `jals.toml` マニフェストが
  `jals build` / `run` / `test` / `clean` / `init` を駆動します。任意の Rhai script は `javac` より先に、
  制限付きの storage-only API だけを使って source を生成し、flag・classpath・environment を追加します。
- **transitive な source-project graph。** `git`/`path` 依存自体を JALS project にできます。stable な
  node identity で diamond を重複排除し、一意な各 node を dependency-first で preprocess してから、
  dependency tree を変更せず検証済み source/classpath artifact だけを投影します。
- **`wasm32` 対応のコア。** エディタ座標変換・構文・フォーマット・lint・セマンティック解析の各層
  （`jals-editor` / `jals-syntax` / `jals-fmt` / `jals-lint` / `jals-hir` / `jals-classfile` /
  `jals-decompile` / `jals-javac` / `jals-storage` / `jals-config`）は `no_std` で
  `wasm32-unknown-unknown` 向けにビルドでき、`jals-classpath` の解決コア、`jals-project` の
  in-memory graph、`jals-build` の Rhai runner も同様です（ホスト I/O は `native` feature の背後に
  あります）。これによりブラウザ playground は同じ解析・project-graph・build-script stack を
  クライアント側だけで動かせます。

## ワークスペース構成

`jals` はブラウザ向け playground を含む 17 個のプロダクト crate からなる Cargo ワークスペースです。

| Crate                                | 説明                                                                                                                                                                                                                                                                                                                                                            |
| ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`jals-editor`](jals-editor)         | definition・references・hover・completion・signature help・highlight の protocol-neutral な意味論と、UTF-8 バイト／UTF-16 座標変換。LSP とブラウザ playground で共有します。                                                                                                                                                                                    |
| [`jals-syntax`](jals-syntax)         | 無損失な Java lexer とエラー耐性のある CST parser（`rowan`）、および CST 上の型付き AST 層。すべてのツールの共通基盤です。                                                                                                                                                                                                                                      |
| [`jals-fmt`](jals-fmt)               | **WIP（作り直し中）。** `jals-syntax` の CST を入力とする Wadler/Prettier 方式の pretty-printer。現状は入力をそのまま返す no-op。                                                                                                                                                                                                                               |
| [`jals-lint`](jals-lint)             | linter（`jals-cli` 経由の `jals lint`）。CST と `jals-hir` に基づくルールレジストリで、欠陥クラス別の 10 section・21 rule を `jalslint.toml` から名前で設定します。rustc/clippy の全 lint を jals rule か「採らない理由」に対応付けた台帳を持ちます。                                                                                                                        |
| [`jals-hir`](jals-hir)               | CST 上での名前解決・ファイル横断の型インデックス・型推論/型検査。linter と LSP が拠り所とするセマンティック層で、コンパイル済み classpath からの外部型の橋渡しも行います。                                                                                                                                                                                      |
| [`jals-classfile`](jals-classfile)   | JVM の `.class` ファイル形式（JVMS 第 4 章）を完全にバイト一致で読み書きするモデル。                                                                                                                                                                                                                                                                            |
| [`jals-decompile`](jals-decompile)   | パース済みの `.class` から読める Java を再構築します。型/シグネチャのレンダリング、初期化子、宣言された `throws`、そして（段階的に）バイトコードからのメソッド本体の完全な逆コンパイル。                                                                                                                                                                        |
| [`jals-javac`](jals-javac)           | コンパイラ本体。Java ソースを実行可能なコードにします。宣言された型ごとの JVM クラスファイル、またはプロジェクト全体を 1 つの WebAssembly モジュール（オブジェクトの管理はホストの GC 任せ）にします。型検査は一切しません（診断は `jals-lint` の担当）が、解決は行います。`invokevirtual` を 1 つ出すには選ばれたオーバーロードとそのディスクリプタが要るためです。 |
| [`jals-classpath`](jals-classpath)   | project byte と検証済み classpath artifact（ローカル/リモート jar、同梱/ネストした jar）を解決・ロードし、`jals-hir`・linter・LSP に供給します。依存に source が無い場合は逆コンパイルした `.java` skeleton にフォールバックします。                                                                                                                            |
| [`jals-config`](jals-config)         | 3 つの設定ファイル（`jals.toml`、`jalsfmt.toml`、`jalslint.toml`）すべての純粋なデータモデル・パース・探索・検証。                                                                                                                                                                                                                                              |
| [`jals-exec`](jals-exec)             | native・browser・inline host 共通の current-thread 実行コンテキスト。確定的な worker fan-out と runtime に依存しない協調 yield を提供します。                                                                                                                                                                                                                   |
| [`jals-storage`](jals-storage)       | revision付きの確定的なproject storage。portable codeは検証済み`FileKey`/`DirKey`、不変`CodeTree` snapshot、transaction、overlay、SHA-256検証付きartifact cacheを使い、memory/native adapterが同じsealed contractを実装します。                                                                                                                                  |
| [`jals-project`](jals-project)       | stable な node identity を持つ transitive path/Git/JAR project graph を探索し、選択 root 直下の正確な `jals.toml` だけを probe し、resolved から preprocessed への phase transition を必須にして、dependency input を node-scoped な検証済み artifact としてのみ `jals-classpath` へ公開します。portable in-memory host と native acquisition host を含みます。 |
| [`jals-build`](jals-build)           | Cargo 風のビルドオーケストレータ。`jals.toml` を `javac`/`java` の計画・clean key・プロジェクト雛形へ変換し、任意の Rhai pre-build script を revision 付き project storage 上で実行します。`jals build`/`run`/`test`/`clean`/`init` と LSP/playground の build phase を支えます。                                                                                      |
| [`jals-lsp`](jals-lsp)               | Language Server Protocol サーバ（`jals lsp` サブコマンド）。同じ CST とセマンティック層から診断・ドキュメントシンボル・整形・hover・定義へのジャンプ・参照検索などを提供。ホスト専用。                                                                                                                                                                          |
| [`jals-progress`](jals-progress)     | 実行中の作業を「データ」として表す語彙。portable な crate はここを通して事実だけを報告し、`--timings` はその台帳を自己完結した HTML ページとして描画する。描画そのものは持たない——事実がどう見えるかはホストが決める。 |
| [`jals-cli`](jals-cli)               | `jals` コマンドラインバイナリ。端末はここが所有する: 出力は単一の `Shell` を必ず通り、cargo 風の表示がイベント列をステータス行とプログレスバーに変える。                                                                                                                                                                                                                                                                                                                                 |
| [`jals-playground`](jals-playground) | [Yew](https://yew.rs) 製・[Trunk](https://trunkrs.dev) でビルドするブラウザ向け playground。`wasm32` にコンパイルし、構文/format/解析/Rhai build-script の各層をブラウザ上だけで動かします。                                                                                                                                                                    |

残り 2 つのワークスペースメンバーは開発専用のツールで、製品には含まれません:
[`jals-tests`](jals-tests)（実世界の Java に対して parser の健全性とフォーマッタの忠実度を
検証するコーパスハーネス）と `xtask`（`cargo xtask codegen` の AST 生成器）です。

```
jals/
├── jals-editor/      # editor query + byte/UTF-16 座標 (no_std, wasm 対応)
├── jals-syntax/      # lexer + CST parser + 型付き AST (no_std, wasm 対応)
├── jals-fmt/         # フォーマッタ — WIP 作り直し中、現状 no-op (no_std, wasm 対応)
├── jals-lint/        # linter (CST + jals-hir 上のルール) (no_std, wasm 対応)
├── jals-hir/         # 名前解決 + 型推論                (no_std, wasm 対応)
├── jals-classfile/   # JVM .class 読み書きモデル        (no_std, wasm 対応)
├── jals-decompile/   # .class -> 読める Java            (no_std, wasm 対応)
├── jals-javac/       # Java -> .class / WasmGC コンパイラ (no_std, wasm 対応)
├── jals-classpath/   # classpath + 依存関係の解決      (no_std + wasm 対応コア)
├── jals-config/      # jals.toml/jalsfmt.toml/jalslint.toml モデル (no_std, wasm 対応)
├── jals-exec/        # current-thread 実行 + worker fan-out (no_std, wasm 対応)
├── jals-progress/    # 実行中の作業をデータ化 + --timings        (no_std, wasm 対応)
├── jals-storage/     # revision付きproject storage      (no_std, wasm 対応)
├── jals-project/     # transitive source-project graph   (no_std + wasm 対応コア)
├── jals-build/       # Cargo 風の javac/java ビルドプランナ (no_std + wasm 対応コア)
├── jals-lsp/         # LSP サーバ (async-lsp, `jals lsp`)  (std, ホスト専用)
├── jals-cli/         # `jals` バイナリ                     (std)
├── jals-playground/  # ブラウザ playground (Yew + Trunk -> wasm)
├── jals-tests/       # コーパステストハーネス (開発専用)
└── xtask/            # codegen 自動化 (開発専用)
```

## インストール

### プリビルドバイナリ（cargo-binstall）

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) は GitHub リリースの資産から
プリビルド済みの `jals` バイナリをダウンロードします（コンパイル不要）:

```sh
cargo binstall --git https://github.com/topi-banana/jals jals-cli
```

### npm / bun

Git リポジトリから直接 `jals` コマンドをインストールできます（Rust ツールチェイン不要）。ランチャーが
初回実行時に、プラットフォームに合ったプリビルドバイナリを GitHub リリースの資産から
ダウンロードします（SHA-256 を検証）:

```sh
bun install -g git+https://github.com/topi-banana/jals.git
# npm の場合:
npm install -g git+https://github.com/topi-banana/jals.git
```

`package.json` の `version` に対応する `v<version>` タグのリリースを解決するため、対応するリリースが
公開されている必要があります。プリビルドバイナリは Linux・macOS・Windows の `x64`/`arm64` 向けに
用意されており、それ以外のプラットフォームではランチャーが `cargo install` を案内します。
`JALS_INSTALL_BASE_URL` を設定するとミラーから資産を取得できます。ランチャーは Node.js 上で動作しますが、
bun は `node` shim を同梱するため、bun のみの環境でもそのまま動きます。

### ソースから（git）

**2024 edition** に対応した Rust ツールチェイン（Rust 1.85 以降、CI は stable でビルド）が必要です。
最新ソースから `jals` をコンパイルします:

```sh
cargo install --git https://github.com/topi-banana/jals jals-cli
```

`jals-cli` というパッケージ名の指定が必要です。これは複数のバイナリを持つ Cargo ワークスペースで、
`cargo install --git` はリポジトリ全体を探索するため、インストールするパッケージを明示しないと選べません。

### ローカルチェックアウトから

```sh
# ワークスペースをビルド
cargo build --release

# `jals` バイナリを ~/.cargo/bin にインストール
cargo install --path jals-cli
```

リリースビルドのバイナリは `target/release/jals` に生成されます。

### GitHub Actions

リポジトリのルート自体が composite action になっているので、ワークフローでは 1 ステップで `jals` を
インストールして `PATH` に通せます:

```yaml
- uses: topi-banana/jals@main
- run: jals fmt --check $(git ls-files '*.java')
- run: jals lint
```

インストール対象は `version` で選びます。既定の `latest` は最新のリリースを解決してプリビルド
バイナリを取得し、併せて公開されている `.sha256` で検証します。`v0.2.0` / `0.2.0` のようなリリース
バージョンを書けばそれに固定されます。それ以外の値（`main`・ブランチ・タグ・コミット SHA）は git ref
として扱われ、リリースアセットが存在しないため `cargo install` でビルドされます。この場合はジョブに
Rust ツールチェインが必要です:

```yaml
- uses: dtolnay/rust-toolchain@stable
- uses: topi-banana/jals@main
  with:
    version: main
```

| 入力          | 既定値                | 意味                                                                                       |
| ------------- | --------------------- | ------------------------------------------------------------------------------------------ |
| `version`     | `latest`              | `latest` / リリースバージョン / ビルドする git ref。                                         |
| `repository`  | `topi-banana/jals`    | アセットとソースの取得元（フォークを指定可能）。                                             |
| `from-source` | `auto`                | `auto` はランナーに合うアセットが無ければビルドへフォールバック。`always` は常にビルド、`never` はビルドせず失敗。 |
| `base-url`    | —                     | GitHub リリースの代わりにミラーのディレクトリからアセットを取得（`JALS_INSTALL_BASE_URL` 相当）。 |
| `cache`       | `true`                | 同じバージョンの導入済みインストールをランナーのツールキャッシュから再利用。                  |
| `token`       | `${{ github.token }}` | `latest` を GitHub API で解決するときにのみ使用。                                            |

出力は `version` / `path` / `bin-dir` / `source`（`prebuilt` か `source`）/ `cache-hit` です。
Linux・macOS・Windows の `x64` / `arm64` ランナーに対応しています。


## 使い方

`jals` はサブコマンド方式で、`fmt`（ソース整形）・`lint`（ソース lint）・`lsp`（language server）
に加え、Cargo 風のビルドフロントエンド（`init` / `build` / `run` / `clean`）があります。

### グローバルオプション

すべてのサブコマンドが共有します。Cargo と同じく、サブコマンドのどちら側に書いても構いません
（`jals --quiet build` と `jals build --quiet` は同じ実行です）。

| オプション | 説明 |
| --- | --- |
| `-q, --quiet` | 警告とエラーだけ。ステータス行もプログレス表示も出しません。 |
| `-v, --verbose` | より多く出します——メモヒット（`Fresh`）、個々のダウンロード、実行前の `javac`/`java` コマンド行。 |
| `--color <auto\|always\|never>` | ANSI カラーを使うかどうか。`auto` では `NO_COLOR` / `CLICOLOR_FORCE` / `TERM=dumb` を尊重します。 |
| `--message-format <human\|json>` | `json` は stdout に 1 行 1 JSON オブジェクトを書きます——表示が描いているのと同じイベント列です。 |
| `--progress <auto\|always\|never>` | ライブのプログレス表示を描くかどうか。`auto` は stderr が端末のときに描きます。 |
| `--timings[=html,json]` | 実行時間の内訳レポートを `target/jals/timings/` に書き出します。値は cargo と同じく `=` で繋ぎます。 |

出力の規則はひとつです。**人間向けは stderr、スクリプト向けは stdout。** そして stdout の持ち主は
つねに一つです。`jals test` は自身の結果オブジェクトのために stdout を保ちます——そこで
`--message-format json` が指してきたのは元々それです——`jals run` も起動したプログラムのために
stdout を明け渡します。一方 `--dry-run` / `--check` / `--diff` /
パイプ入力の `jals fmt` はいずれも stdout に自前の成果物を書きます。同じ行に二つ目のスキーマを
混ぜる代わりに、`json` との併用は拒否されます。

実行は cargo と同じ体裁で自身を語り（`Preparing` / `Resolving` / `Downloaded` / `Extracting` /
`Remapping` / `Decompiling` / `Indexing` / `Compiling` / `Packaging` / `Fresh` / `Finished`）、
各行はそれがどのパッケージについてかを示します。stderr が端末なら作業単位ごとにプログレスバーが
出ます。ダウンロードは個別に告げるのではなくフェーズごとに 1 行へ集約され、`-v` で 1 件ずつに
戻ります:

```console
$ jals build --features 1.21.6
   Preparing hellomod v0.1.0
   Preparing minecraft v0.1.0
  Downloaded 2 files (58.1 MiB) in 2.5s
  Extracting minecraft v0.1.0 (META-INF/versions/26.2/server-26.2.jar)
     Merging minecraft v0.1.0
 Decompiling [00:00:41] [=========>          ] 8213/29184 minecraft v0.1.0 (net/minecraft)
  Publishing minecraft v0.1.0 (minecraft-26.2)
   Compiling hellomod v0.1.0
   Remapping hellomod v0.1.0 (1 class)
   Packaging hellomod v0.1.0 (target/jals/remap/hellomod-0.1.0.jar)
    Finished `default` profile in 184.02s
```

`--timings` は自己完結した HTML ページ——作業単位ごとのバー、並列度のプロット、アクティビティ別の
内訳——と、その隣に上書きされる `jals-timings.html` を書きます。`cargo build --timings` と同じ運用です。

### ファイルをその場でフォーマット

```sh
# 個別のファイルをフォーマット
jals fmt src/Main.java src/Util.java

# ディレクトリツリーをフォーマット（*.java を再帰的に探索）
jals fmt src/
```

### stdin/stdout でフォーマット

パスを指定しない場合、ソースは stdin から読み込まれ、整形結果は stdout へ書き出されます。

```sh
cat Main.java | jals fmt
```

### check モード（CI 向け）

`--check` は何も書き込まず、変更が生じるファイルが 1 つでもあれば非ゼロで終了します。整形対象に
なるファイルは stderr に一覧表示されます。

```sh
jals fmt --check src/
```

フォーマッタが**自分の出力を拒否した**場合も失敗します。fail-safe がレイアウトを拒否して入力を
そのまま返した状態で、ファイルはバイト単位で同一なのに整形されていません。これはソースではなく
`jals-fmt` 側のバグなので、そのように報告されます。`--check` の問いは「すべてのファイルが整形済みか」
であり、このファイルは整形されていないので失敗します。警告として報告されるため、モードに関わらず
`-D warnings` でも失敗します。

### 構文警告をエラーとして扱う

フォーマッタは不正な入力に対してもベストエフォートで動作します（CST が無損失なので整形は続行され
ます）。`-D warnings` を渡すと、構文エラーがあった時点で実行を失敗させられます。

```sh
jals fmt -D warnings src/
```

### ファイルを lint する

```sh
# 個別のファイルを lint
jals lint src/Main.java src/Util.java

# ディレクトリツリーを lint（*.java を再帰的に探索）
jals lint src/
```

`jals lint` は **10 section・21 rule** を、名前解決と型推論（`jals-hir`）を使って検出します。単なる
構文木上のパターンマッチではありません。解決できない名前・型不一致・報告されていない検査例外
（`[correctness]`）、`[package] features` に応じたプレビュー機能と方言構文（`[compatibility]`）、
定数条件による到達不能分岐と握り潰された例外（`[suspicious]`）、未使用の束縛・import・`private`
メンバ（`[unused]`）、そして `[complexity]` / `[performance]` / `[style]` / `[naming]` /
`[documentation]` / `[restriction]` の各 rule です。すべて名前付きの rule なので、`cannot-resolve`
を含めどれでも severity 変更・無効化ができます。`jals.toml` マニフェストが見つかれば、その `[build]
classpath` と `[dependencies]` も解決されるため、外部ライブラリの型も理解されます。

設定は `jalslint.toml`（`jalsfmt.toml` と同じ方法で探索されます）で行います。rule の値は level か、
level と rule 固有の option をまとめた table です。

```toml
[unused]
dead-code = "allow"

[style]
missing-braces = { level = "warn", policy = "multi-line" }
```

rule 一覧・設定リファレンス・rustc/clippy の残りの lint を移植する roadmap は
[`jals-lint/README.md`](jals-lint/README.md) に、両ツールの全 1,059 lint を分類した台帳は
[`jals-lint/MAPPING-rustc-clippy.md`](jals-lint/MAPPING-rustc-clippy.md) にあります。

> **`jalslint.toml` の形が変わりました。** フラットな `[rules]` table は無くなり、rule はそれが
> 報告する欠陥クラスの section の下に書きます（`[rules] wildcard-import = "allow"` →
> `[style] wildcard-import = "allow"`）。この jals が定義していない key は**拒否せず保持**され
> ます — 古い名前 1 つでファイル全体が読めなくなってはいけないからです — が、黙って消えることも
> なく、`jals lint` が `warning: <file>: unknown lint key <key>` を出します。かつての
> `[rules] unused-local` は `unused-variables` / `unused-imports` / `dead-code` に分割済みで、
> 避けようのない引数のための抑制は `[unused] unused-variables` です。

### language server を起動する

`jals lsp` は stdio 上で LSP サーバを起動し、エディタ統合（診断（lint の診断を含む）・ドキュメント
シンボル・hover・定義へのジャンプ・参照検索・全体整形）を提供します。いずれも同じ CST とセマン
ティック層から得られます。手動ではなくエディタから起動される想定です。エディタ設定は
[`jals-lsp`](jals-lsp/README.md) を参照してください。

```sh
jals lsp
```

### Java プロジェクトをビルドする（Cargo 風）

`jals` はソースツールにとどまらず、JDK に対する小さな Cargo 風フロントエンドでもあります。
`Cargo.toml` の Java 版にあたる [`jals.toml`](jals-build/README.md) マニフェストに、ソースの場所・
コンパイル済みクラスの出力先・ターゲットにする Java release・classpath を宣言すると、ビルド
サブコマンドがそれを `javac`/`java` の起動コマンドへと変換します。

```sh
jals init my-app            # ./my-app に雛形を生成（jals.toml, src/main/java/Main.java, .gitignore）
cd my-app
jals build                  # javac でコンパイル
jals build --dry-run        # コンパイルせず javac コマンドを表示
jals run                    # コンパイルしてから [run] main-class を実行
jals run -- arg1 arg2       # ...プログラムへ引数を渡す
jals test                   # `#[test]` メソッドを 1 テスト 1 JVM で実行
jals test --list            # 実行せずにテスト一覧を表示
jals clean                  # ビルド出力（target/classes・target/test-classes）を削除
```

最小の `jals.toml`（すべてのキーは任意で、省略時は Maven 風の `src/main/java` → `target/classes`
レイアウトになります）:

```toml
[package]
name = "hello"
version = "0.1.0"

# `script` が `build.feature("…")` で読む Cargo 風の build feature。
# `--features` / `--all-features` / `--no-default-features` で選択し、選択は加法的です。
# Cargo と同じく package ごと: dependency へ渡るのは manifest が明示した分だけです
# （下の `<dep>/<feature>`、または [dependencies] の `features`）。
# [features]
# default = ["server"]
# server  = []
# client  = []
# gpu     = ["render/vulkan"]   # dependency `render` の `vulkan` を有効化（Cargo の `serde/std`）

[build]
release = 21                        # javac --release N
# source-dirs = ["src/main/java"]   # -sourcepath のルート。.java 探索の対象でもある
# classes-dir = "target/classes"    # javac -d
# classpath   = ["libs/guava.jar"]  # -classpath エントリ
# script = { type = "rhai", file = "build.rhai" }

[run]
main-class = "com.example.Main"     # `jals run` のエントリポイント

[dependencies]
# source project は transitive に探索され、`dir` で monorepo 内の project を選択する
shared = { path = "../shared" }
core = { git = "https://github.com/example/mono", rev = "abc123", dir = "core" }
# `features` はその dependency 自身の build.rhai で有効になる build feature（Cargo と同じ）。
# `default-features = false` でその dependency 自身の `default` リストを適用しない
render = { path = "../render", features = ["vulkan"], default-features = false }
```

`script` を設定すると、`build.rhai` は source 探索と `javac` より先に実行されます。project snapshot と
選択された `[features]` を読み、通常の生成物を `target/jals/build/rhai/out` 以下へ書き、
生成 source・classpath entry・`javac`/JVM
flag・compile/run environment entry を追加できます。さらに型付き `tasks` DAG で、size/digest 検証付き
download、JSON projection、安全な sources JAR 展開、mappings による jar remap（Mojang/ProGuard・tiny v2）、jar merge、
compile 向け decompile、排他的な物理 source tree の publish を宣言できます。
Rhai は task 結果を読めず process も起動しません。`replace-root` は宣言した destination 以下を全置換し、
通常出力と同じ transaction で publish されます。native CLI と LSP は task を実行し、LSP は destination
内に open document があれば延期します。browser は物理 publish を fetch 前に拒否します。完全な API、fingerprint/cache、
sandbox limit、Rust の `BuildScript` model は
[`jals-build` の Rhai reference](jals-build/README.md#rhai-build-scripts)を、実行可能な例は
[`examples/rhai_build_script`](examples/rhai_build_script)を参照してください。
source archive task の形は [`examples/task_source_archive`](examples/task_source_archive)、
remap 済み Minecraft の例は [`examples/minecraft`](examples/minecraft)
にあります。その上に Mixin mod を組み立てる例が
[`examples/minecraft_mod`](examples/minecraft_mod) で、宣言的な `[mappings]` の代替と
`[build] remap` により全 43 リリース向けに jar を package し、そのうち難読化された 39
リリースでは再難読化します。source tree は 43 リリースで 1 つです。その範囲内で Mojang が
rename した唯一の API を dialect の `#[cfg]` が引き受け、その述語である threshold feature の
chain は build script と resource template も読みます。この mod の `jals test` は実際の Minecraft
client を起動して assert します。しかも同じ 43 リリースのいずれでもです。それを行う harness は
`[dev-dependencies]` に 1 行書くだけの別 project —
[`examples/minecraft_client_test`](examples/minecraft_client_test) — で、build は解決せず jar にも
入りません。この harness が各リリースの runtime jar 約 60 本を pin し、client API 用の threshold
chain を自前で持つため、mod 側の test はリリース名を一切書きません。

root Rhai phase 自体は capability 制限されていますが、その compiler/JVM 引数、classpath、subprocess
environment directive は、後続の明示的な `jals build` / `run` による JDK process へ意図的に反映され
ます。信頼していない checkout をビルドする前に、project code と同様に root build script を確認して
ください。

この portable phase 以外では `jals-build` がコマンドをデータとして計画し、マニフェスト探索・source
走査・JDK 起動を `jals-cli` が担います（`javac`/`java` は `$JAVAC`/`$JAVA`、次に
`$JAVA_HOME/bin`、最後に `PATH` の順で解決します）。

### Transitive な project dependency

`path` または `git` dependency の root は、宣言した directory/checkout に、指定されていれば `dir` を
続けた場所です。`jals-project` は `<selected-root>/jals.toml` だけを probe し、上位 directory は探索
しません。その file があれば node は JALS project となり、child dependency・`[build] classpath`・
`[build] source-dirs` はすべてその selected root を基準に解決されます。file が無ければ従来の source
規約（`src/main/java`、次に `src`、最後に selected root）を使います。file が存在するのに不正な場合や
dependency cycle がある場合、`jals build`/`run` は hard failure になります。

graph node は stable identity を持つため、dependency 名が異なる diamond でも一度だけ visit されます。
一意な各 node は dependency-first 順に、無条件かつちょうど一度 preprocess transition を通ります。
binary node と legacy-source node では no-op、manifest-backed node では任意の Rhai script を実行します。
dependency script が export するのは `build.add_source` で登録した source と
`build.add_classpath` で登録した classpath だけです。`javac`/JVM argument、compile/run environment、
metadata は node-local のままで伝播しません。output・classpath entry・source snapshot は node identity
の下へ digest 検証済み artifact として publish され、dependency source tree は変更されません。root
script は、process argument/environment と revision-check 付き root output 更新を含む従来の完全な
semantics を維持します。

native CLI の `build`/`run` は graph 全体を使い、transitive source を compile して transitive JAR と
宣言 classpath を追加します。`lint` は binary/classpath 側を解決しつつ、指定された file だけを lint
します。LSP は source artifact を解析/navigation 用に index し、local path root を watch します。
hard graph error は root manifest に診断してから root-only analysis へ fallback します。playground は
一つの in-memory `CodeTree` 上で portable な `MemoryProjectGraph` を動かすため、tree 内の path project
と script を browser でも利用できます。一方 browser では Git を取得できません。Git entry は warning
を出して省略され、browser Git support を提供するものではありません。

この transitive JALS source-project graph は実装済みです。Maven/POM coordinate resolution、coordinate
version selection、transitive Maven download、`jals.lock` lockfile は将来の課題です。

### オプション

| オプション        | 説明                                                                                                                             |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `[PATHS]...`      | フォーマット対象のファイルまたはディレクトリ。ディレクトリは `.java` ファイルを再帰的に探索します。パス指定なし → stdin/stdout。 |
| `--check`         | 何も書き込まず、変更が生じるファイル、またはフォーマッタが自分の出力を拒否したファイルがあれば非ゼロで終了します。               |
| `-D <LINT>`       | lint を拒否（繰り返し指定可）。認識されるのは `warnings` のみで、構文警告のあるファイル、または formatter が自身の出力を拒否したファイルがあれば失敗します。 |
| `--config <PATH>` | `jalsfmt.toml` の探索の代わりに、指定した設定ファイルを使用します。                                                              |
| `--no-migrate`    | 検出した既存フォーマッタ設定から `jalsfmt.toml` を生成しません。検出した設定はその実行では引き続き使われます。                   |

## 設定

フォーマッタは `jalsfmt.toml` を読み込みます。CLI は、整形する各ファイルのディレクトリから上位
方向に探索して見つけます（`--config <PATH>` で特定のファイルを指定することも可能）。設定は
**セクション**の集合（`[layout]`、`[blank-lines]`、`[braces]`、`[wrapping]`、`[spacing]`、
`[comments]`、`[imports]`、`[literals]`）で、すべてのキーは、そしてセクションごと省略しても、
デフォルト値が使われます。キーは kebab-case です。

```toml
# jalsfmt.toml — すべてのキーは任意。以下の値はデフォルト。
[layout]
indent-style = "space" # "space" | "tab" | "mixed"
indent-width = 4       # インデント 1 段あたりの桁数
tab-width = 4          # タブ文字の表示幅
max-width = 100        # 列上限
line-ending = "auto"   # "lf" | "crlf" | "auto" | "native"
insert-final-newline = true

[blank-lines]
max-in-code = 1 # 本体中に残す、ソース由来の連続空行の上限

[imports]
order = "sort"     # "preserve" | "sort" | "group"

[comments]
format-javadoc = false
width = 80 # 再整形キーが on のときだけ参照される
```

各セクションのよく触るキーは `jals-fmt/jalsfmt.toml` に、全体は `jals-config/src/fmt/` の各
セクションモジュールに記載しています。各キーが Eclipse / IntelliJ / google-java-format /
Spotless のどの設定に対応するかは `jals-fmt/MAPPING.md` の台帳を参照してください。

### 既存のフォーマッタ設定を移行する

`jalsfmt.toml` が無いとき、`jals fmt` と `jals init` はプロジェクトが既に持っているフォーマッタ
設定を探し、jals のオプションへ翻訳して書き出します。判定は**ファイル名ではなく内容**で行い
（Eclipse のエクスポートプロファイルや IntelliJ のスキームは任意の名前を取りうるため）、最初に
一致したものが採用されます:

1. `.editorconfig`
2. `.idea/codeStyles/Project.xml`、またはトップレベルの `*.xml` のうち `<code_scheme>` を持つもの
3. トップレベルの `*.xml` のうち Eclipse フォーマッタプロファイルを持つもの
4. `.settings/org.eclipse.jdt.core.prefs`

生成される `jalsfmt.toml` には**デフォルトと異なるキーだけ**が、由来を記録したヘッダの下に
書き出されます:

```toml
# Generated by jals from .settings/org.eclipse.jdt.core.prefs (eclipse).

[layout]
indent-width = 2
max-width = 120
```

原則:

- **既存の `jalsfmt.toml` は決して書き換えません。** 当該ディレクトリまたはその祖先に 1 つでも
  あれば、そこで探索を打ち切ります。
- **探索はプロジェクトルートで止まります**（`jals.toml` か `.git` を持つ最も近い祖先）。どちらも
  無いディレクトリツリーはプロジェクトとみなさず、移行も書き出しも行いません。
- **`--check` / `--diff` / 標準入力では書き出しません**が、移行した設定は使います。CI の
  `--check` が通常実行と同じ結果を出すためです。`--config <PATH>` を渡した場合は移行自体を
  行わず、`--no-migrate` は設定を使ったうえで書き出しだけを抑止します。
- **Spotless は検出しません。** その設定は Gradle/Maven のビルド DSL、つまりデータではなく
  コードであり、さらに委譲先のエンジンを選択するため、値を確実に読み取れません。

翻訳先は jals の共通語彙であって全単射ではありません。jals に等価物が無いネイティブオプションは
持ち越されず、逆に jals 側で複数セクションに分かれているものはそのすべてに書き出されます（例えば
IntelliJ の `RIGHT_MARGIN` は `layout.max-width` と `comments.width` の両方になります）。そのため、
元の設定に無いキーが生成ファイルに現れることがあります。意図的に写さないものは `jals-fmt/MAPPING.md` §7 に、仕組み全体は
`jals-fmt/DESIGN.md` §15 に記録しています。

### 例

入力:

```java
package a.b;import java.util.List;public class Foo{private int x=1;void m(int a){if(a>0){foo(a);}return;}}
```

`jals fmt` の出力:

```java
package a.b;
import java.util.List;
public class Foo {
    private int x = 1;
    void m(int a) {
        if (a > 0) {
            foo(a);
        }
        return;
    }
}
```

## Playground

`jals-playground` は小さなブラウザアプリ（[Yew](https://yew.rs) 製、[Trunk](https://trunkrs.dev)
でビルド・配信）で、`wasm32` にコンパイルした構文・format・解析・sandbox 化された Rhai build-script
の各層を、サーバを介さずブラウザ上だけで動かします。生成 Java source・remote jar・`jals.toml` の
portable な in-memory path-project graph もブラウザ内で解決するため、hover / 補完 / 型検査がそれらを
認識できます。browser は Git dependency を clone できず、Git support を提供すると見なさず各 entry
を warning として報告します。

*Build* は in-process の `jals-javac` でワークスペースをコンパイルし（JDK もサブプロセスも不要）、
成果物をダウンロードとして提供します。`[build] backend = { type = "jals" }` は宣言された型ごとの
class file を実行可能な `.jar` にパッケージし、`{ type = "jals-wasm" }` はプロジェクト全体を 1 つの
WebAssembly module として出力します（manifest の既定値 `{ type = "javac" }` はブラウザタブに起動
できるプロセスがないため、その旨を報告します）。コンパイラは classpath ではなく `jals-hir` の埋め込み
JDK stub に対して解決するので、解決済みの `[dependencies]` jar は editor の classpath には載っても
コンパイラの classpath には載りません。まだ lowering のない構文は誤ったコードを吐かず、Build output
タブに*報告*されます。

```sh
# 初回のみ: wasm ターゲットと Trunk を用意
rustup target add wasm32-unknown-unknown
cargo install trunk

# ライブリロード付きで配信（デフォルトは http://0.0.0.0:8000）
cd jals-playground
trunk serve
```

ブラウザ向けバンドルは Trunk が `wasm32` 向けに生成します。`jals-playground` は通常の
ワークスペースメンバーでもあるため、ホスト向けの `cargo build` / `clippy` / `test` でもビルドされます。

## ライブラリとして使う

これらの crate はまだ crates.io へ公開されていません。git またはパス指定で依存に追加してください。

### `jals-syntax`

```rust
use jals_syntax::{tokenize, SyntaxKind};

// 字句解析: 各トークンの text を連結すると入力に一致する（lossless）。
let tokens = tokenize("int x = 1;");
assert_eq!(tokens[0].kind, SyntaxKind::INT_KW);

// CST 上の型付き AST ビューへとパースする。
use jals_syntax::ast::{AstNode, SourceFile};
let parse = jals_syntax::parse("class Foo { }");
let file = SourceFile::cast(parse.syntax()).unwrap();
let class = file.decls().next().unwrap();
assert_eq!(class.syntax().text().to_string(), "class Foo { }");
```

### `jals-fmt`

> **WIP — フォーマッタはゼロから作り直し中で、現状は no-op です。**
> `FormatOutput::format_source` は入力をバイト単位でそのまま返します（parser の構文エラーだけ
> を warning として surface）。`Config` は受け取りますが無視します。以下の例は、作り直しが完了
> した後の想定される形です。

```rust
use jals_config::fmt::Config;
use jals_fmt::FormatOutput;

let out = FormatOutput::format_source("class C{int x=1;}", &Config::default()).await;
// 想定（実装後）: "class C {\n    int x = 1;\n}\n"。
// 現状（no-op）: out.formatted == "class C{int x=1;}"。
assert!(!out.has_warnings());
```

## アーキテクチャ

```
ソース ──▶ lexer (手書き) ──▶ CST parser (rowan) ──▶ 型付き AST
            lossless           エラー耐性               (jals-syntax)
                                    │
                                    ▼
                         CST を lower ──▶ Doc IR ──▶ render ──▶ 整形済みテキスト
                                         Wadler/Prettier          (jals-fmt)
```

- **Lexer**（`jals-syntax`）: 手書きのスキャナ。トリビア（空白・改行・コメント）も実
  トークンとして出力するためストリームは無損失です。文脈依存キーワード（`var` / `record` /
  `sealed` / `when`、module ディレクティブなど）は識別子として字句化し、parser が昇格させます。
- **Parser**（`jals-syntax`）: 手書きの再帰下降パーサ。イベント列を出力し、それを `rowan` の
  green tree へ組み立てます。エラーからは回復し、中断せずに `SyntaxError` として記録します。
- **型付き AST**（`jals-syntax`）: CST 上のゼロコストな newtype ビュー。利用側は生の kind を
  マッチするのではなく、型付きアクセサ経由で木を読みます。
- **Formatter**（`jals-fmt`）: **WIP — ゼロから作り直し中で、現状は no-op です。** 想定される
  設計では、CST を Wadler/Prettier 方式のドキュメント IR へ lower し、各グループが 1 行に収まる
  か改行すべきかを判断しながら render します。
- **Project graph**（`jals-project`）: stable identity を持つ transitive path/Git/JAR node を探索し、
  selected root 直下の正確な manifest だけを probe し、assembly より前の preprocess を type-level
  transition として必須にします。assembly は graph metadata を公開しますが、consumer へ渡す
  authored source、script-registered source/classpath は node-scoped な検証済み artifact だけです。
  native acquisition host と一つの `CodeTree` を使う portable memory host がこの深い interface を共有
  します。

## 開発

```sh
cargo build --workspace
cargo test  --workspace --all-features
```

CI（GitHub Actions）は以下のチェックを実行します。push する前にローカルでも同じものを回してください。

```sh
cargo fmt --all --check                                       # 整形
cargo run -p xtask -- codegen --check                         # 生成された AST が最新か
cargo clippy --workspace --all-targets --all-features -- -D warnings   # lint
cargo test --workspace --all-features                         # テスト
taplo fmt --check --diff                                      # TOML の整形
cargo machete                                                 # 未使用の依存
typos                                                         # スペルチェック
ast-grep test --skip-snapshot-tests                           # ast-grep ルールのテスト
ast-grep scan --error                                         # 構造的な lint（no-free-functions など）
cargo check -p jals-project --no-default-features             # portable project-graph core
cargo check -p jals-project --all-features                    # native path/Git acquisition

# wasm: pure な `no_std` クレート群（1 つのパッケージ集合としてビルドし `std` feature を無効に保つ)…
cargo build --release --target wasm32-unknown-unknown \
  -p jals-editor -p jals-syntax -p jals-classfile -p jals-hir -p jals-decompile \
  -p jals-javac -p jals-fmt -p jals-lint -p jals-storage -p jals-config
# … に加えて jals-classpath の wasm 対応コア（ホスト I/O はデフォルトの `native` feature の背後）
cargo build --release --target wasm32-unknown-unknown -p jals-classpath --no-default-features
# portable in-memory project graph は dependency-script preparation と artifact projection を含む
cargo check -p jals-project --no-default-features --target wasm32-unknown-unknown
# Rhai feature はホスト I/O を持たず wasm 対応で、browser host も同じ engine をビルドする
cargo check -p jals-build --no-default-features --features rhai --target wasm32-unknown-unknown
cargo build -p jals-playground --target wasm32-unknown-unknown
```

lint はルートの `Cargo.toml` の `[workspace.lints]` でワークスペース全体に設定しており（clippy の
`all` / `pedantic` / `nursery` を `warn`、CI では deny）、構造的なルールは `.ast-grep/rules/` に
置いています。ビルドマトリクスでは `x86_64` / `aarch64` Linux 向けにもワークスペースをコンパイル
します。依存関係の更新は Dependabot で自動化されています。

主要な構造ルールである `no-free-functions` は、ヘルパーを free function ではなく associated
function（あるいはネストした関数）にすることを求めます。ここでは抽象化を最優先事項として扱って
います — 抽象化はコードベース全体の品質を高め、パフォーマンスの向上にも大きく寄与しうるため、
free function はできる限り避けます。associated function は親となる型を見るだけで、その関数が何に
関連し何を行うかを呼び出し側が一目で判別できます。これは特に外部 import 経由で使われる `pub` な
関数で重要で、素の free function にはそうした手掛かりがありません。関数を特定の struct にまとめて
配置すると、似たようなヘルパーの重複にも気付きやすく、統合しやすくなります。ヘルパーは
`impl` / `trait` に移すか、その関数だけが呼び出すローカルなものであれば呼び出し元の内側にネスト
してください。

### 守るべき不変条件

以下の性質はテスト（`proptest` によるプロパティテストを含む）で保証されており、構文層やフォー
マット層への変更でも維持されなければなりません。

- lexer は無損失で、panic しない。
- parser は常に木を返し、panic しない。
- フォーマッタは、`jals_fmt::passes::token_license::OPERATIONS`（`jals-fmt/DESIGN.md` §20）に宣言された
  操作が適用される箇所を除いて意味のあるトークンの多重集合を保持し、コメントを削除・並べ替えせず、
  冪等である。fail-safe はその表を読み、保証できない実行では入力をそのまま返す。
- `jals-editor` / `jals-syntax` / `jals-fmt` / `jals-lint` / `jals-hir` / `jals-classfile` /
  `jals-decompile` / `jals-javac` / `jals-storage` / `jals-config` は `no_std` crate として
  `wasm32-unknown-unknown` 向けにビルドできる。
  `jals-classpath` の解決コア（`--no-default-features`）と、portable な `rhai` feature を有効にした
  `jals-build`、`jals-project` の in-memory graph も `wasm32` 向けにビルドできる。

## ステータス

初期段階（`0.1.0`）です。フォーマッタ・linter・language server は動作し、構文層は Java の広い
範囲をカバーしていますが、API は変更される可能性があります。セマンティック解析（`jals-hir`）は
名前解決・ファイル横断の型インデックス・型推論/型検査をカバーしており、プロジェクトの classpath
や `[dependencies]` から解決した型も扱えますが、ジェネリックメソッドの型推論・より高度な
バイトコード逆コンパイル（ループの `break`/`continue`、try-with-resources）・Maven 座標
（`group:artifact:version`）の POM/version 解決と lockfile はまだ未対応です。transitive な JALS
`path`/`git` source-project graph は実装済みで、より広い Maven dependency management・テスト・
パッケージングは build [ロードマップ](jals-build/README.md#roadmap)上にあります。

## ライセンス

以下のいずれか

- Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) または
  <http://www.apache.org/licenses/LICENSE-2.0>）
- MIT ライセンス（[LICENSE-MIT](LICENSE-MIT) または <http://opensource.org/licenses/MIT>）

を選択してご利用いただけます。

明示的に別段の表明をしない限り、あなたが本作品に意図的に提出した貢献（Apache-2.0 ライセンスの定義による）
は、追加の条項や条件なしに、上記のとおりデュアルライセンスされるものとします。
