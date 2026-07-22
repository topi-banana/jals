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
フロントエンド（`jals build` / `run` / `clean` / `init`）も備えています。

> The English README is available at [README.md](README.md).

## 特長

- **無損失かつエラー耐性。** lexer は入力の全バイトをちょうど 1 トークンに対応させ、parser は
  不正な入力に対しても必ず木を返します。どちらも panic しません。
- **Java 26 文法に対応。** class / interface / enum / record、sealed 型、アノテーション、lambda、
  switch 式、パターン（record パターンや guard を含む）などをサポートします。
- **保証付きのフォーマッタ。** 意味のあるトークンは決して変更せず、コメントを削除・並べ替え
  することもなく、冪等です（`format(format(x)) == format(x)`）。
- **本物のセマンティクスを持つ linter。** 構文的なチェックにとどまらず、`jals lint` は名前解決と
  型推論を CST 上で行い、未使用のローカル変数・型不一致・報告されていない検査例外・到達しない
  条件分岐を検出します（単なるパターンマッチではありません）。
- **Cargo 風の Java ビルド。** `Cargo.toml` の Java 版にあたる `jals.toml` マニフェストが
  `jals build` / `run` / `clean` / `init` を駆動します。任意の Rhai script は `javac` より先に、
  制限付きの storage-only API だけを使って source を生成し、flag・classpath・environment を追加します。
- **transitive な source-project graph。** `git`/`path` 依存自体を JALS project にできます。stable な
  node identity で diamond を重複排除し、一意な各 node を dependency-first で preprocess してから、
  dependency tree を変更せず検証済み source/classpath artifact だけを投影します。
- **`wasm32` 対応のコア。** エディタ座標変換・構文・フォーマット・lint・セマンティック解析の各層
  （`jals-editor` / `jals-syntax` / `jals-fmt` / `jals-lint` / `jals-hir` / `jals-classfile` /
  `jals-decompile` / `jals-storage` / `jals-config`）は `no_std` で `wasm32-unknown-unknown` 向けに
  ビルドでき、`jals-classpath` の解決コア、`jals-project` の in-memory graph、`jals-build` の Rhai
  runner も同様です（ホスト I/O は `native` feature の背後にあります）。これによりブラウザ
  playground は同じ解析・project-graph・build-script stack をクライアント側だけで動かせます。

## ワークスペース構成

`jals` はブラウザ向け playground を含む 16 個のプロダクト crate からなる Cargo ワークスペースです。

| Crate | 説明 |
| --- | --- |
| [`jals-editor`](jals-editor) | definition・references・hover・completion・signature help・highlight の protocol-neutral な意味論と、UTF-8 バイト／UTF-16 座標変換。LSP とブラウザ playground で共有します。 |
| [`jals-syntax`](jals-syntax) | 無損失な Java lexer とエラー耐性のある CST parser（`rowan`）、および CST 上の型付き AST 層。すべてのツールの共通基盤です。 |
| [`jals-fmt`](jals-fmt) | `jals-syntax` の CST を入力とする Wadler/Prettier 方式の pretty-printer。 |
| [`jals-lint`](jals-lint) | linter（`jals-cli` 経由の `jals lint`）。CST と `jals-hir` に基づくルールレジストリで、未使用のローカル変数・型不一致・報告されていない例外・定数条件による到達不能分岐・`[package] features` に応じたプレビュー機能チェックを行います。 |
| [`jals-hir`](jals-hir) | CST 上での名前解決・ファイル横断の型インデックス・型推論/型検査。linter と LSP が拠り所とするセマンティック層で、コンパイル済み classpath からの外部型の橋渡しも行います。 |
| [`jals-classfile`](jals-classfile) | JVM の `.class` ファイル形式（JVMS 第 4 章）を完全にバイト一致で読み書きするモデル。 |
| [`jals-decompile`](jals-decompile) | パース済みの `.class` から読める Java を再構築します。型/シグネチャのレンダリング、初期化子、宣言された `throws`、そして（段階的に）バイトコードからのメソッド本体の完全な逆コンパイル。 |
| [`jals-classpath`](jals-classpath) | project byte と検証済み classpath artifact（ローカル/リモート jar、同梱/ネストした jar）を解決・ロードし、`jals-hir`・linter・LSP に供給します。依存に source が無い場合は逆コンパイルした `.java` skeleton にフォールバックします。 |
| [`jals-config`](jals-config) | 3 つの設定ファイル（`jals.toml`、`jalsfmt.toml`、`jalslint.toml`）すべての純粋なデータモデル・パース・探索・検証。 |
| [`jals-exec`](jals-exec) | native・browser・inline host 共通の current-thread 実行コンテキスト。確定的な worker fan-out と runtime に依存しない協調 yield を提供します。 |
| [`jals-storage`](jals-storage) | revision付きの確定的なproject storage。portable codeは検証済み`FileKey`/`DirKey`、不変`CodeTree` snapshot、transaction、overlay、SHA-256検証付きartifact cacheを使い、memory/native adapterが同じsealed contractを実装します。 |
| [`jals-project`](jals-project) | stable な node identity を持つ transitive path/Git/JAR project graph を探索し、選択 root 直下の正確な `jals.toml` だけを probe し、resolved から preprocessed への phase transition を必須にして、dependency input を node-scoped な検証済み artifact としてのみ `jals-classpath` へ公開します。portable in-memory host と native acquisition host を含みます。 |
| [`jals-build`](jals-build) | Cargo 風のビルドオーケストレータ。`jals.toml` を `javac`/`java` の計画・clean key・プロジェクト雛形へ変換し、任意の Rhai pre-build script を revision 付き project storage 上で実行します。`jals build`/`run`/`clean`/`init` と LSP/playground の build phase を支えます。 |
| [`jals-lsp`](jals-lsp) | Language Server Protocol サーバ（`jals lsp` サブコマンド）。同じ CST とセマンティック層から診断・ドキュメントシンボル・整形・hover・定義へのジャンプ・参照検索などを提供。ホスト専用。 |
| [`jals-cli`](jals-cli) | `jals` コマンドラインバイナリ。 |
| [`jals-playground`](jals-playground) | [Yew](https://yew.rs) 製・[Trunk](https://trunkrs.dev) でビルドするブラウザ向け playground。`wasm32` にコンパイルし、構文/format/解析/Rhai build-script の各層をブラウザ上だけで動かします。 |

残り 2 つのワークスペースメンバーは開発専用のツールで、製品には含まれません:
[`jals-tests`](jals-tests)（実世界の Java に対して parser の健全性とフォーマッタの忠実度を
検証するコーパスハーネス）と `xtask`（`cargo xtask codegen` の AST 生成器）です。

```
jals/
├── jals-editor/      # editor query + byte/UTF-16 座標 (no_std, wasm 対応)
├── jals-syntax/      # lexer + CST parser + 型付き AST (no_std, wasm 対応)
├── jals-fmt/         # フォーマッタ (CST -> Doc IR -> テキスト) (no_std, wasm 対応)
├── jals-lint/        # linter (CST + jals-hir 上のルール) (no_std, wasm 対応)
├── jals-hir/         # 名前解決 + 型推論                (no_std, wasm 対応)
├── jals-classfile/   # JVM .class 読み書きモデル        (no_std, wasm 対応)
├── jals-decompile/   # .class -> 読める Java            (no_std, wasm 対応)
├── jals-classpath/   # classpath + 依存関係の解決      (no_std + wasm 対応コア)
├── jals-config/      # jals.toml/jalsfmt.toml/jalslint.toml モデル (no_std, wasm 対応)
├── jals-exec/        # current-thread 実行 + worker fan-out (no_std, wasm 対応)
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

## 使い方

`jals` はサブコマンド方式で、`fmt`（ソース整形）・`lint`（ソース lint）・`lsp`（language server）
に加え、Cargo 風のビルドフロントエンド（`init` / `build` / `run` / `clean`）があります。

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

`jals lint` は未使用のローカル変数・型不一致・報告されていない検査例外・定数条件による到達不能
分岐、`[package] features` に応じたプレビュー機能を、名前解決と型推論（`jals-hir`）を使って検出します。単なる
構文木上のパターンマッチではありません。`jals.toml` マニフェストが見つかれば、その `[build]
classpath` と `[dependencies]` も解決されるため、外部ライブラリの型も理解されます。設定は
`jalslint.toml`（`jalsfmt.toml` と同じ方法で探索されます）で行います。

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
jals clean                  # ビルド出力（target/classes）を削除
```

最小の `jals.toml`（すべてのキーは任意で、省略時は Maven 風の `src/main/java` → `target/classes`
レイアウトになります）:

```toml
[package]
name = "hello"
version = "0.1.0"

# `script` が `build.feature("…")` で読む Cargo 風の build feature。
# `--features` / `--all-features` / `--no-default-features` で選択し、選択は加法的です。
# Cargo と同じく package ごと: dependency には伝播しません（[dependencies] の `features` を使う）。
# [features]
# default = ["server"]
# server  = []
# client  = []

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
# `features` はその dependency 自身の build.rhai で有効になる build feature（Cargo と同じ）
render = { path = "../render", features = ["vulkan"] }
```

`script` を設定すると、`build.rhai` は source 探索と `javac` より先に実行されます。project snapshot と
選択された `[features]` を読み、通常の生成物を `target/jals/build/rhai/out` 以下へ書き、
生成 source・classpath entry・`javac`/JVM
flag・compile/run environment entry を追加できます。さらに型付き `tasks` DAG で、size/digest 検証付き
download、JSON projection、安全な sources JAR 展開、Mojang mappings による jar remap、jar merge、
compile 向け decompile、排他的な物理 source tree の publish を宣言できます。
Rhai は task 結果を読めず process も起動しません。`replace-root` は宣言した destination 以下を全置換し、
通常出力と同じ transaction で publish されます。native CLI と LSP は task を実行し、LSP は destination
内に open document があれば延期します。browser は物理 publish を fetch 前に拒否します。完全な API、fingerprint/cache、
sandbox limit、Rust の `BuildScript` model は
[`jals-build` の Rhai reference](jals-build/README.md#rhai-build-scripts)を、実行可能な例は
[`examples/rhai_build_script`](examples/rhai_build_script)を参照してください。
source archive task の形は [`examples/task_source_archive`](examples/task_source_archive)、
remap 済み Minecraft の例は [`examples/minecraft-mojang-remap`](examples/minecraft-mojang-remap)
にあります。

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

| オプション | 説明 |
| --- | --- |
| `[PATHS]...` | フォーマット対象のファイルまたはディレクトリ。ディレクトリは `.java` ファイルを再帰的に探索します。パス指定なし → stdin/stdout。 |
| `--check` | 何も書き込まず、変更が生じるファイルがあれば非ゼロで終了します。 |
| `-D <LINT>` | lint を拒否（繰り返し指定可）。認識されるのは `warnings` のみで、構文警告のあるファイルがあれば失敗します。 |
| `--config <PATH>` | `jalsfmt.toml` の探索の代わりに、指定した設定ファイルを使用します。 |

## 設定

フォーマッタは `jalsfmt.toml` を読み込みます。CLI は、整形する各ファイルのディレクトリから上位
方向に探索して見つけます（`--config <PATH>` で特定のファイルを指定することも可能）。すべてのキーは
任意で、省略時はデフォルト値が使われます。キーは kebab-case です。

```toml
# jalsfmt.toml — すべてのキーは任意。以下の値はデフォルト。
indent-style = "space"      # "space" | "tab"
indent-width = 4
max-blank-lines = 1         # 連続する空行をこの数まで圧縮する
line-ending = "lf"          # "lf" | "crlf"
insert-final-newline = true
max-width = 100             # コードの折り返し目標（桁数）
comment-width = 80          # コメント / Javadoc の再整形目標（桁数）
```

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

```rust
use jals_fmt::{Config, format_source};

let out = format_source("class C{int x=1;}", &Config::default());
assert_eq!(out.formatted, "class C {\n    int x = 1;\n}\n");
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
- **Formatter**（`jals-fmt`）: CST を Wadler/Prettier 方式のドキュメント IR へ lower し、各
  グループが 1 行に収まるか改行すべきかを判断しながら render します。
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
  -p jals-fmt -p jals-lint -p jals-storage -p jals-config
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
- フォーマッタは意味のあるトークン列を保持し、コメントを削除・並べ替えせず、冪等である。
- `jals-editor` / `jals-syntax` / `jals-fmt` / `jals-lint` / `jals-hir` / `jals-classfile` /
  `jals-decompile` / `jals-storage` / `jals-config` は `no_std` crate として
  `wasm32-unknown-unknown` 向けにビルドできる。
  `jals-classpath` の解決コア（`--no-default-features`）と、portable な `rhai` feature を有効にした
  `jals-build`、`jals-project` の in-memory graph も `wasm32` 向けにビルドできる。

## ステータス

初期段階（`0.1.0`）です。フォーマッタ・linter・language server は動作し、構文層は Java の広い
範囲をカバーしていますが、API は変更される可能性があります。セマンティック解析（`jals-hir`）は
名前解決・ファイル横断の型インデックス・型推論/型検査をカバーしており、プロジェクトの classpath
や `[dependencies]` から解決した型も扱えますが、ジェネリックメソッドの型推論・より高度な
バイトコード逆コンパイル（`switch`/`try`-`catch`/`break`/`continue`）・Maven 座標
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
