# jals

[![CI](https://github.com/topi-banana/jals/actions/workflows/ci.yml/badge.svg)](https://github.com/topi-banana/jals/actions/workflows/ci.yml)

**lossless（無損失）な構文木**を基盤にした、Rust 製の Java ツールチェインです。

`jals` は Java のソースを完全忠実な CST（具象構文木）へとパースします。空白やコメントを含む
すべてのバイトが保持され、その木を土台にソースツールを構築します。現在はコードフォーマッタ・
linter・language server（LSP）を提供しており、いずれも名前解決・ファイル横断の型インデックス・
型推論/型検査を行う共通のセマンティック層（`jals-hir`）に支えられています。この層はプロジェクトの
コンパイル済み classpath や `[dependencies]`（ローカル/リモート、`git`/`path` の jar。ソース jar
が無ければ逆コンパイルして読める Java を生成）から型を解決することもできます。これらと並んで、
`jals.toml` マニフェストから JDK の `javac` / `java` をラップする Cargo 風のビルドフロントエンド
（`jals build` / `run` / `clean` / `init`）も備えています。

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
  `jals build` / `run` / `clean` / `init` を駆動します。これは薄く純粋な `javac`/`java`
  ラッパーで、コマンドをデータとして組み立てるだけで、CLI が実行するまで JDK には一切触れません。
- **`wasm32` 対応のコア。** エディタ座標変換・構文・フォーマット・lint・セマンティック解析の各層
  （`jals-editor` / `jals-syntax` / `jals-fmt` / `jals-lint` / `jals-hir` / `jals-classfile` /
  `jals-decompile` / `jals-storage` / `jals-config`）は `no_std` で `wasm32-unknown-unknown` 向けに
  ビルドでき、`jals-classpath` の
  解決コアも同様です（ホスト I/O は `native` feature の背後にあります）。これによりブラウザ
  playground は同じ解析スタックをクライアント側だけで動かせます。

## ワークスペース構成

`jals` はブラウザ向け playground を含む 14 個のプロダクト crate からなる Cargo ワークスペースです。

| Crate | 説明 |
| --- | --- |
| [`jals-editor`](jals-editor) | definition・references・hover・completion・signature help・highlight の protocol-neutral な意味論と、UTF-8 バイト／UTF-16 座標変換。LSP とブラウザ playground で共有します。 |
| [`jals-syntax`](jals-syntax) | 無損失な Java lexer とエラー耐性のある CST parser（`rowan`）、および CST 上の型付き AST 層。すべてのツールの共通基盤です。 |
| [`jals-fmt`](jals-fmt) | `jals-syntax` の CST を入力とする Wadler/Prettier 方式の pretty-printer。 |
| [`jals-lint`](jals-lint) | linter（`jals-cli` 経由の `jals lint`）。CST と `jals-hir` に基づくルールレジストリで、未使用のローカル変数・型不一致・報告されていない例外・定数条件による到達不能分岐・`[package] features` に応じたプレビュー機能チェックを行います。 |
| [`jals-hir`](jals-hir) | CST 上での名前解決・ファイル横断の型インデックス・型推論/型検査。linter と LSP が拠り所とするセマンティック層で、コンパイル済み classpath からの外部型の橋渡しも行います。 |
| [`jals-classfile`](jals-classfile) | JVM の `.class` ファイル形式（JVMS 第 4 章）を完全にバイト一致で読み書きするモデル。 |
| [`jals-decompile`](jals-decompile) | パース済みの `.class` から読める Java を再構築します。型/シグネチャのレンダリング、初期化子、宣言された `throws`、そして（段階的に）バイトコードからのメソッド本体の完全な逆コンパイル。 |
| [`jals-classpath`](jals-classpath) | プロジェクトの classpath と `[dependencies]`（ローカル/リモートの jar、同梱/ネストした jar、`git`/`path` のソース依存）を解決・ロードし、`jals-hir`・linter・LSP に供給します。依存にソースが無い場合は逆コンパイルした `.java` スケルトンにフォールバックします。 |
| [`jals-config`](jals-config) | 3 つの設定ファイル（`jals.toml`、`jalsfmt.toml`、`jalslint.toml`）すべての純粋なデータモデル・パース・探索・検証。 |
| [`jals-storage`](jals-storage) | revision付きの確定的なproject storage。portable codeは検証済み`FileKey`/`DirKey`、不変`CodeTree` snapshot、transaction、overlay、SHA-256検証付きartifact cacheを使い、memory/native adapterが同じsealed contractを実装します。 |
| [`jals-build`](jals-build) | Cargo 風のビルドオーケストレータ。`jals.toml` マニフェストを解析し、`javac`/`java` のコマンド計画・clean 対象パス・プロジェクト雛形へと変換します。すべて純粋なデータで、`jals-syntax` への依存も I/O もありません。`jals build`/`run`/`clean`/`init` を支えます。 |
| [`jals-lsp`](jals-lsp) | Language Server Protocol サーバ（`jals lsp` サブコマンド）。同じ CST とセマンティック層から診断・ドキュメントシンボル・整形・hover・定義へのジャンプ・参照検索などを提供。ホスト専用。 |
| [`jals-cli`](jals-cli) | `jals` コマンドラインバイナリ。 |
| [`jals-playground`](jals-playground) | [Yew](https://yew.rs) 製・[Trunk](https://trunkrs.dev) でビルドするブラウザ向け playground。`wasm32` にコンパイルし、構文/フォーマット/解析の各層をブラウザ上だけで動かします。 |

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
├── jals-storage/     # revision付きproject storage      (no_std, wasm 対応)
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

[build]
release = 21                        # javac --release N
# source-dirs = ["src/main/java"]   # -sourcepath のルート。.java 探索の対象でもある
# classes-dir = "target/classes"    # javac -d
# classpath   = ["libs/guava.jar"]  # -classpath エントリ

[run]
main-class = "com.example.Main"     # `jals run` のエントリポイント
```

ビルド crate（`jals-build`）はコマンドを純粋なデータとして*計画する*だけです。マニフェストの探索・
ソースの走査・JDK ツールの起動は `jals-cli` が担います（`javac`/`java` は `$JAVAC`/`$JAVA`、次に
`$JAVA_HOME/bin`、最後に `PATH` の順で解決します）。マニフェストの完全なリファレンスと、より本格的な
Cargo-for-Java フロントエンドに向けたロードマップは [`jals-build/README.md`](jals-build/README.md)
を参照してください。

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
でビルド・配信）で、`wasm32` にコンパイルした構文層・フォーマット層・解析層を、サーバを介さず
ブラウザ上だけで動かします。`jals.toml` の `[dependencies]` もブラウザ内で解決するため、hover /
補完 / 型検査が外部ライブラリの型を認識できます。

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

# wasm: pure な `no_std` クレート群（1 つのパッケージ集合としてビルドし `std` feature を無効に保つ)…
cargo build --release --target wasm32-unknown-unknown \
  -p jals-editor -p jals-syntax -p jals-classfile -p jals-hir -p jals-decompile \
  -p jals-fmt -p jals-lint -p jals-storage -p jals-config
# … に加えて jals-classpath の wasm 対応コア（ホスト I/O はデフォルトの `native` feature の背後）
cargo build --release --target wasm32-unknown-unknown -p jals-classpath --no-default-features
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
  `jals-classpath` の解決コアも `wasm32` 向けにビルドできる（`--no-default-features`）。

## ステータス

初期段階（`0.1.0`）です。フォーマッタ・linter・language server は動作し、構文層は Java の広い
範囲をカバーしていますが、API は変更される可能性があります。セマンティック解析（`jals-hir`）は
名前解決・ファイル横断の型インデックス・型推論/型検査をカバーしており、プロジェクトの classpath
や `[dependencies]` から解決した型も扱えますが、ジェネリックメソッドの型推論・より高度な
バイトコード逆コンパイル（`switch`/`try`-`catch`/`break`/`continue`）・Maven 座標
（`group:artifact:version`）形式の依存解決はまだ未対応です。`jals build`/`run`/`clean`/`init`
フロントエンドは、現状は忠実ながら薄い `javac`/`java` ラッパーであり、より本格的な依存関係管理・
テスト・パッケージングは[ロードマップ](jals-build/README.md#roadmap)上にあります。

## ライセンス

以下のいずれか

- Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) または
  <http://www.apache.org/licenses/LICENSE-2.0>）
- MIT ライセンス（[LICENSE-MIT](LICENSE-MIT) または <http://opensource.org/licenses/MIT>）

を選択してご利用いただけます。

明示的に別段の表明をしない限り、あなたが本作品に意図的に提出した貢献（Apache-2.0 ライセンスの定義による）
は、追加の条項や条件なしに、上記のとおりデュアルライセンスされるものとします。
