# jals

[![CI](https://github.com/topi-banana/jals/actions/workflows/ci.yml/badge.svg)](https://github.com/topi-banana/jals/actions/workflows/ci.yml)

**lossless（無損失）な構文木**を基盤にした、Rust 製の Java ツールチェインです。

`jals` は Java のソースを完全忠実な CST（具象構文木）へとパースします。空白やコメントを含む
すべてのバイトが保持され、その木を土台にソースツールを構築します。現在はコードフォーマッタと
language server（LSP）を提供しており、同じ基盤の上に linter も載せられるよう設計されています。
これらと並んで、`jals.toml` マニフェストから JDK の `javac` / `java` をラップする Cargo 風の
ビルドフロントエンド（`jals build` / `run` / `clean` / `init`）も備えています。

> The English README is available at [README.md](README.md).

## 特長

- **無損失かつエラー耐性。** lexer は入力の全バイトをちょうど 1 トークンに対応させ、parser は
  不正な入力に対しても必ず木を返します。どちらも panic しません。
- **Java 26 文法に対応。** class / interface / enum / record、sealed 型、アノテーション、lambda、
  switch 式、パターン（record パターンや guard を含む）などをサポートします。
- **保証付きのフォーマッタ。** 意味のあるトークンは決して変更せず、コメントを削除・並べ替え
  することもなく、冪等です（`format(format(x)) == format(x)`）。
- **Cargo 風の Java ビルド。** `Cargo.toml` の Java 版にあたる `jals.toml` マニフェストが
  `jals build` / `run` / `clean` / `init` を駆動します。これは薄く純粋な `javac`/`java`
  ラッパーで、コマンドをデータとして組み立てるだけで、CLI が実行するまで JDK には一切触れません。
- **`wasm32` 対応のコア。** CLI を除くすべてが `wasm32-unknown-unknown` 向けにビルドできるため、
  構文層とフォーマット層をブラウザ上で動かせます。

## ワークスペース構成

`jals` は 5 つのコア crate とブラウザ向け playground からなる Cargo ワークスペースです。

| Crate | 説明 |
| --- | --- |
| [`jals-syntax`](jals-syntax) | 無損失な Java 26 lexer（`logos`）とエラー耐性のある CST parser（`rowan`）、および CST 上の型付き AST 層。すべてのツールの共通基盤です。 |
| [`jals-fmt`](jals-fmt) | `jals-syntax` の CST を入力とする Wadler/Prettier 方式の pretty-printer。 |
| [`jals-lsp`](jals-lsp) | Language Server Protocol サーバ（`jals lsp` サブコマンド）。同じ CST から診断・ドキュメントシンボル・整形を提供。ホスト専用。 |
| [`jals-build`](jals-build) | Cargo 風のビルドオーケストレータ。`jals.toml` マニフェストを解析し、`javac`/`java` のコマンド計画・clean 対象パス・プロジェクト雛形へと変換します。すべて純粋なデータで、`jals-syntax` への依存も I/O もありません。`jals build`/`run`/`clean`/`init` を支えます。 |
| [`jals-cli`](jals-cli) | `jals` コマンドラインバイナリ。 |
| [`jals-playground`](jals-playground) | [Yew](https://yew.rs) 製・[Trunk](https://trunkrs.dev) でビルドするブラウザ向け playground。`wasm32` にコンパイルし、`jals-syntax` / `jals-fmt` をブラウザ上だけで動かします。 |

```
jals/
├── jals-syntax/      # lexer + CST parser + 型付き AST  (wasm 対応)
├── jals-fmt/         # フォーマッタ (CST -> Doc IR -> テキスト)
├── jals-lsp/         # LSP サーバ (async-lsp, `jals lsp`)
├── jals-build/       # Cargo 風の javac/java ビルドプランナ  (wasm 対応)
├── jals-cli/         # `jals` バイナリ
└── jals-playground/  # ブラウザ playground (Yew + Trunk -> wasm)
```

## インストール

**2024 edition** に対応した Rust ツールチェイン（Rust 1.85 以降、CI は stable でビルド）が必要です。

```sh
# ワークスペースをビルド
cargo build --release

# `jals` バイナリを ~/.cargo/bin にインストール
cargo install --path jals-cli
```

リリースビルドのバイナリは `target/release/jals` に生成されます。

## 使い方

`jals` はサブコマンド方式で、`fmt`（ソース整形）・`lsp`（language server）に加え、Cargo 風の
ビルドフロントエンド（`init` / `build` / `run` / `clean`）があります。

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

### language server を起動する

`jals lsp` は stdio 上で LSP サーバを起動し、エディタ統合（診断・ドキュメントシンボル・全体整形）
を提供します。いずれも同じ CST から得られます。手動ではなくエディタから起動される想定です。
エディタ設定は [`jals-lsp`](jals-lsp/README.md) を参照してください。

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
でビルド・配信）で、`wasm32` にコンパイルした構文層・フォーマット層を、サーバを介さずブラウザ上
だけで動かします。

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
ソース ──▶ lexer (logos) ──▶ CST parser (rowan) ──▶ 型付き AST
            lossless           エラー耐性               (jals-syntax)
                                    │
                                    ▼
                         CST を lower ──▶ Doc IR ──▶ render ──▶ 整形済みテキスト
                                         Wadler/Prettier          (jals-fmt)
```

- **Lexer**（`jals-syntax`）: `logos` ベースのスキャナ。トリビア（空白・改行・コメント）も実
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
cargo clippy --workspace --all-targets --all-features -- -D warnings   # lint
cargo test --workspace --all-features                         # テスト
taplo fmt --check --diff                                      # TOML の整形
cargo machete                                                 # 未使用の依存
cargo build --release --target wasm32-unknown-unknown -p jals-syntax   # wasm コア
```

ビルドマトリクスでは `x86_64` / `aarch64` Linux 向けにもワークスペースをコンパイルします。依存
関係の更新は Dependabot で自動化されています。

### 守るべき不変条件

以下の性質はテスト（`proptest` によるプロパティテストを含む）で保証されており、構文層やフォー
マット層への変更でも維持されなければなりません。

- lexer は無損失で、panic しない。
- parser は常に木を返し、panic しない。
- フォーマッタは意味のあるトークン列を保持し、コメントを削除・並べ替えせず、冪等である。
- `jals-syntax`（および `jals-fmt`）は `wasm32-unknown-unknown` 向けにビルドできる。

## ステータス

初期段階（`0.1.0`）です。フォーマッタと language server は動作し、構文層は Java 26 の広い範囲を
カバーしていますが、API は変更される可能性があります。`jals build`/`run`/`clean`/`init`
フロントエンドは、現状は忠実ながら薄い `javac`/`java` ラッパーであり、依存関係管理・テスト・
パッケージングは[ロードマップ](jals-build/README.md#roadmap)上にあります。構文層の次の利用者
としては linter（`jals-lint`）を想定しています。

## ライセンス

ライセンスはまだ定められていません。設定されるまでは、すべての権利を作者が留保します。
