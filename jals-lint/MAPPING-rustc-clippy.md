# rustc / clippy lint → jals rule 写像

> **この文書の位置づけ**: 「**rustc_lint と clippy の rule を、Rust 固有のものを除いて全て実装する**」
> という方針に対し、**両ツールが出荷する全 lint** について「jals のどの rule になるのか／ならないなら
> 何故か」を 1 行 1 lint で確定した台帳である。`jals-fmt/MAPPING-rustfmt.md` の rustc/clippy 版であり、
> **バケツ記号 (M / X / R / D / C / N) はそちらの §2 をそのまま流用している** — 記号を作り直すと、
> あちらで既に一度説明した X と R の線引きを、ここでもう一度説明し直す羽目になるからである。
>
> 一次情報から機械抽出した**完全な lint 目録**は、本書の付随データとしてリポジトリに入っている:
> - `jals-lint/inventory-rustc.tsv` — 244 lint
> - `jals-lint/inventory-clippy.tsv` — 815 lint
>
> この 2 ファイルは飾りではなく、`jals-lint/tests/inventory.rs` が読む入力である（§8）。
>
> **roadmap（N をどう実装していくか）は `jals-lint/README.md` にある。** 本書は「何が N か」を
> 決める台帳であって、実装順序は決めない。

---

## 0. 抽出元の固定

写像表は一次情報から機械的に抽出したものであり、抽出点を固定しておかないと再検証できない。

| 項目 | 値 |
|---|---|
| 抽出日 | 2026-08-19 |
| toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)` / `clippy 0.1.97 (8bab26f4f6 2026-07-14)` |
| 抽出コマンド | `clippy-driver -Whelp` |
| rustc lint 一覧 | 出力の "Lint checks provided by rustc" — **244 lint** |
| clippy lint 一覧 | 出力の "Lint checks loaded by this crate" — **815 lint** |
| clippy group | 出力の "Lint groups loaded by this crate"（`clippy::all` を除く 9 group、排他） |
| jals 側 | `jals_lint::RuleInfo::all()` — 10 section **19 rule**（`jals-lint/tests/registry.rs` が固定） |

**なぜ `clippy-driver -Whelp` なのか。** clippy の lint 定義はソースツリー上では `declare_clippy_lint!`
マクロに散っていて、group 所属は宣言の第 1 引数にある。ドライバに聞けば「このバージョンが実際に
持っている lint と group」が返るので、ソースを読んで数え直すより再現性が高く、toolchain の版数だけで
抽出点が固定される。

**数え方**: 母数は 244 + 815 = **1,059**。deprecated / renamed の別名は `-Whelp` に出ないので数えない
（出ないものを数えると母数が toolchain から再導出できなくなる）。§2 のバケツの合計は 1,059 に一致する
（一致しない表は完全性の台帳として無価値）。

---

## 1. 前史 — jals-lint は「Java の lint」から始まった

`jals-lint` は rustc/clippy の移植として書かれたのではない。最初にあったのは Java 固有の 15 rule で、
`unused-variables` / `unused-imports` / `dead-code` の 3 分割だけが「rustc の対応 rule に倣った」もの
だった（root `README.md` にその経緯がある）。

本作業はその向きを逆にする。**両ツールの全 lint を母数に取り、jals 側に何が無いかを数える**。結果として
分かったことが 2 つある。

1. 既存 19 rule のうち **10 rule には rustc/clippy の祖先が無い**（§6）。方言の feature gate 4 つ、
   Java の意味論そのものである `[correctness]` 3 つ、そして `constant-condition` / `empty-catch` /
   `missing-braces`。これは「移植し忘れ」ではなく、**Java の lint には Rust の lint に対応物が無い領域が
   ある**という事実である。
2. 移植可能で未実装のものが **376 行 = 286 rule** ある（§7）。既存 19 rule の 15 倍であり、
   `jalslint.toml` の schema をこの規模に耐える形に作り直したのは、この数を見たからである。

---

## 2. 分類

全 1,059 lint を 6 バケツに割る。**未実装は N だけ**で、他の 5 つは答えが出ている。

| 記号 | 意味 | 数 |
|---|---|---|
| **M** | 既存 jals rule に写る | 16 |
| **X** | Rust 固有（**機構レベル**）— edition / ABI / const-eval / toolchain | 32 |
| **R** | Rust 固有（**構文名指し**） | 582 |
| **D** | 意図的に不採用（典拠あり） | 36 |
| **C** | source の lint ではない（lint 機構 / driver 出力 / `Cargo.toml`） | 17 |
| **N** | 移植可能で、**未実装**（roadmap） | 376 |
| | **合計** | **1,059** |

内訳の内訳:

| 出典 | M | X | R | D | C | N | 計 |
|---|---|---|---|---|---|---|---|
| rustc | 8 | 32 | 144 | 4 | 7 | 49 | 244 |
| clippy | 8 | 0 | 438 | 32 | 10 | 327 | 815 |

**X が rustc 側に偏るのは偶然ではない。** clippy は「書き方」の lint を出すが、edition 移行・ABI・
const 評価・target 固有の制約は言語実装そのものの都合であり、それを言うのは rustc の仕事だからである。

---

## 3. 全 1,059 lint

行そのものは TSV にある。本書に 1,059 行の表を写しても、TSV と二重管理になるだけで読めるようにはならない。

- `jals-lint/inventory-rustc.tsv` — `lint <TAB> rustc 既定 <TAB> bucket <TAB> jals rule <TAB> note`
- `jals-lint/inventory-clippy.tsv` — `lint <TAB> clippy group <TAB> clippy 既定 <TAB> bucket <TAB> jals rule <TAB> note`

**多対一は正しい形である。** 「排他的な rule の組は 1 つの enum にする」という設計方針（§7）の結果、
複数の source lint が同じ jals rule を指す行が出る。その場合も**行は source lint ごとに 1 行**であり、
同じ jals rule 名が複数行に出る。合算して 1 行にまとめると母数が 1,059 に一致しなくなり、完全性の台帳
としての価値が消える。

---

## 4. X と R の線引き

`jals-fmt/MAPPING-rustfmt.md` §2 と同じ基準を使う。すなわち:

- **R** = lint が **Java に対応物の無い構文/型を名指ししている**。`transmute`、lifetime、borrow、
  `unsafe`、raw pointer、trait、closure、macro、`Option`/`Result`/`Vec`/`Rc`/`Cow`、iterator adaptor、
  `?` 演算子。「Java に書き換えたら別の rule になる」ものはここ。
- **X** = 構文ではなく **機構が Rust 固有**。edition 移行 (`rust-2021-*` / `rust-2024-*`)、ABI と
  calling convention、const 評価の時間制限、`#[feature]` の状態、target 固有の ABI 問題、GPU kernel。
  Java にも「リリース間の非互換」はあるが、それは jals では `[compatibility]` section の関心事であり、
  edition という機構の写しではない。

**境界例を 2 つ明示しておく。**

- `rustc::while-true` は R でも X でもなく **D** である。Java にも `while (true)` は書けるので構文の
  問題ではない。採らないのは、**Java には `loop` に当たる代替表現が無く、`while (true)` が唯一の
  無限ループの書き方だから**である。同じ判断を `jals-hir` の `dead_ifs` が既にしていて、`if` 文しか
  見ない理由がそこにある。
- `rustc::unexpected-cfgs` と `rustc::ill-formed-attribute-input` は **M** で、写り先は rule ではなく
  固定診断 `cfg` である。jals の `#[cfg(...)]` は方言構文なので「Rust 固有」ではないが、構造的に壊れた
  attribute はビルドを止めるエラーであって設定可能な lint ではない。台帳がその写り先を名指しできる
  ように、`tests/inventory.rs` は `cfg` を「実装済みの名前」に含めている。

---

## 5. M — 既存 jals rule に写る（16）

| lint | clippy group | jals rule |
|---|---|---|
| `rustc::dead-code` | - | `dead-code` |
| `rustc::non-camel-case-types` | - | `naming-convention` |
| `rustc::non-snake-case` | - | `naming-convention` |
| `rustc::non-upper-case-globals` | - | `naming-convention` |
| `rustc::unexpected-cfgs` | - | `cfg` |
| `rustc::unused-imports` | - | `unused-imports` |
| `rustc::unused-variables` | - | `unused-variables` |
| `rustc::ill-formed-attribute-input` | - | `cfg` |
| `clippy::enum-glob-use` | pedantic | `wildcard-import` |
| `clippy::print-stderr` | restriction | `print-to-console` |
| `clippy::print-stdout` | restriction | `print-to-console` |
| `clippy::wildcard-imports` | pedantic | `wildcard-import` |
| `clippy::box-default` | style | `boxed-primitive-constructor` |
| `clippy::collapsible-if` | style | `collapsible-if` |
| `clippy::empty-docs` | suspicious | `empty-javadoc` |
| `clippy::extra-unused-type-parameters` | complexity | `unused-variables` |

**逆向き**: 既存 19 rule のうち、ここに現れないのは次の 10 個。いずれも rustc/clippy に祖先が無い
jals 固有の rule であり、`tests/inventory.rs` の `JALS_NATIVE` がその一覧を保持している
（新しい rule を「祖先無し」にするのは、そこへの明示的な追記を要求する）。

| jals rule | なぜ祖先が無いか |
|---|---|
| `attribute` / `compact-source-file` / `grouped-import` / `module-import` | jals 方言と Java preview 機能の feature gate。gate する方言を持つ処理系が他に無い |
| `cannot-resolve` / `type-mismatch` / `unreported-exception` | Java の意味論そのもの（名前解決・代入可能性・検査例外）。rustc では型検査であって lint ではない |
| `constant-condition` | rustc の `unconditional-panic` 等は別の事実を見ている。定数畳み込みで死ぬ分岐を報告する lint は両ツールに無い |
| `empty-catch` | Rust に `catch` 節が無い |
| `missing-braces` | Rust は本体が常にブロックで、省略できない |

---

## 6. D — 意図的に不採用（36）

**D は「Java に書けない」ではない**（それは R）。**書けるが、採らない理由がある**行である。理由は必ず
TSV の `note` 列に入っており、`tests/inventory.rs` が「D 行に理由が空でないこと」を強制する。

| lint | clippy group | 不採用の理由 |
|---|---|---|
| `rustc::missing-debug-implementations` | - | Java の既定 toString は常に存在し、全型への override は慣習でない |
| `rustc::while-true` | - | `while (true)` は Java 唯一の無限ループ表現。constant-condition が意図的に除外している |
| `rustc::overflowing-literals` | - | javac 自身が拒否する。lint が言い直しても何も足さない |
| `rustc::useless-deprecated` | - | `@Deprecated` の無効な位置は javac の -Xlint:dep-ann が既に扱い、jals が二重に言う価値がない |
| `clippy::doc-markdown` | pedantic | Javadoc は HTML であって markdown ではなく、記法違反という概念がない |
| `clippy::empty-structs-with-brackets` | restriction | Java の空クラス本体 `{}` は必須構文で、省略形が存在しない |
| `clippy::format-collect` | pedantic | `map(...).collect(joining())` は Java の慣用形で、置き換え先がない |
| `clippy::items-after-statements` | pedantic | Java のローカルクラス/初期化ブロックは文の間に書くのが通常で、位置に規約がない |
| `clippy::missing-fields-in-debug` | pedantic | `toString` の内容は規約でなく、フィールドの網羅を求める根拠がない |
| `clippy::module-name-repetitions` | restriction | Java のパッケージ名を型名に繰り返すのは `com.example.user.UserService` のように通常の命名 |
| `clippy::naive-bytecount` | pedantic | Java に `bytecount` に当たる最適化 API がなく、置き換え先がない |
| `clippy::pathbuf-init-then-push` | restriction | Java の `Path.of(a, b, c)` は可変長引数で、段階的 `resolve` は別用途 |
| `clippy::redundant-clone` | nursery | Java の防御的コピーは意図であり、不要と判定する根拠が構文に現れない |
| `clippy::should-panic-without-expect` | pedantic | JUnit の `assertThrows` は例外型を必須にしており、同じ穴が空かない |
| `clippy::stable-sort-primitive` | pedantic | Java のプリミティブ配列 `Arrays.sort` は既に不安定ソートで、選び直す余地がない |
| `clippy::string-lit-chars-any` | restriction | Java の同型は `"ab".indexOf(c) >= 0` で、既にそれが最短形 |
| `clippy::unnecessary-trailing-comma` | pedantic | Java の配列初期化子/列挙定数の末尾カンマは慣用形で、jals-fmt が扱う |
| `clippy::cmp-null` | style | Java で null 比較は `== null` が唯一の書き方であり、置き換え先がない |
| `clippy::field-reassign-with-default` | style | Java には初期化子付き構築式がなく、生成後の setter 呼び出しは通常の書き方 |
| `clippy::filter-next` | complexity | Java Stream に `find(pred)` はなく、`filter().findFirst()` が唯一の書き方 |
| `clippy::manual-checked-ops` | complexity | Java に `checked_*` に当たる API がなく、ゼロ検査は正しい書き方のまま |
| `clippy::manual-clear` | perf | `truncate` に当たる API が Java の `List` になく、対応する誤用形が存在しない |
| `clippy::manual-is-multiple-of` | complexity | `x % n == 0` は Java の慣用形で、置き換え先の API がない |
| `clippy::manual-main-separator-str` | complexity | Java は `File.separator`（文字列）と `separatorChar` を両方公開しており、誤用形が存在しない |
| `clippy::manual-pattern-char-comparison` | style | Java の同型は正規表現文字クラスで、置き換えが読みやすさを損なう |
| `clippy::manual-range-contains` | style | Java 標準に区間型がなく、置き換え先が存在しない |
| `clippy::manual-saturating-arithmetic` | style | Java に飽和演算がない（`Math.addExact` は例外を投げる別物） |
| `clippy::manual-split-once` | complexity | Java の `split(re, 2)` が既にその形であり、置き換え先がない |
| `clippy::manual-strip` | complexity | Java に `stripPrefix`/`stripSuffix` に当たる API がない |
| `clippy::manual-while-let-some` | style | `while (!q.isEmpty()) { q.poll(); }` は Java の慣用形で、置き換え先がない |
| `clippy::mixed-case-hex-literals` | style | jals-fmt の `[literals] hex-case` が既に正規化する。lint が重ねて言う理由がない |
| `clippy::needless-as-bytes` | complexity | Java の `getBytes().length` は符号化に依存し `length()` と一致しない。置換は不健全 |
| `clippy::needless-splitn` | complexity | Java の `split` の limit は挙動（末尾空要素）を変えるので冗長ではない |
| `clippy::deprecated-semver` | correctness | Java の `@Deprecated(since=)` は自由文字列で、semver を検査する根拠がない |
| `clippy::read-line-without-trim` | correctness | Java の `BufferedReader.readLine` は改行を含まないので同じ罠が起きない |
| `clippy::suspicious-splitn` | correctness | Java の `split(re, 0)` は既定動作であって誤用ではない |

理由は 4 種類に分かれる。

1. **置き換え先が Java に無い** — `manual-strip`、`manual-range-contains`、`manual-saturating-arithmetic`。
   「もっと良い書き方がある」と言う lint は、その書き方が標準ライブラリに存在して初めて意味を持つ。
2. **Java では既にその形が最短** — `filter-next`、`cmp-null`、`string-lit-chars-any`、`manual-split-once`。
3. **javac / 既存ツールが既に言う** — `overflowing-literals`、`useless-deprecated`。lint が言い直しても
   何も足さない。
4. **jals の別レイヤの担当** — `mixed-case-hex-literals`（`jals-fmt` の `[literals] hex-case`）、
   `unnecessary-trailing-comma`（同じく formatter）。**lint と formatter が同じことを言うと、片方を
   直したときにもう片方が残る**。

---

## 7. N — 376 行が 286 rule に畳まれる

移植可能で未実装の 376 行は、jals の rule としては **286 個**になる。畳まれる理由は 3 つあり、
どれも `jalslint.toml` の schema 設計そのものである。

### 7.1 排他的な組は 1 つの enum key になる

clippy が「複数の lint を任意の組み合わせで有効化する」形で表現しているものは、**到達不能な状態を
持つ**。1 key 1 enum に畳むと、到達可能な状態は全て表せて、到達不能なものは表せなくなる。

| clippy の組 | jals の 1 key |
|---|---|
| `print_stdout` / `print_stderr` | `print-to-console` の `streams = "both" \| "stdout" \| "stderr"`（**実装済み**） |
| `shadow_same` / `shadow_reuse` / `shadow_unrelated` | `shadowed-name` の `kinds` |
| `big_endian_bytes` / `little_endian_bytes` / `host_endian_bytes` | `byte-order` |
| `mutex_atomic` / `mutex_integer` | `mutex-for-atomic` |
| `integer_division` / `integer_division_remainder_used` | `integer-division` |
| `fn_params_excessive_bools` / `struct_excessive_bools` | `excessive-booleans` |
| `missing_docs` / `missing_docs_in_private_items` | `missing-javadoc` の可視性 key |
| `missing_errors_doc` / `missing_panics_doc` / `missing_safety_doc` | `missing-javadoc-tag` |

**組が常に 2 個とは限らない**（endian は 3 個）。だから bool ではなく enum を作る、というのが
`jals_config::lint` の設計原則である。

### 7.2 同じ Java の事実を別角度から見ているものは 1 rule になる

`rustc::unused-results` と `rustc::unused-must-use` は「戻り値が捨てられている」という 1 つの事実で、
Java では `unused-return-value` 1 個。`clippy::eq_op` と `clippy::erasing_op` は別の rule だが、
`clippy::cast_possible_truncation` と `cast_precision_loss` は Java ではどちらも「縮小変換」で
`narrowing-cast` 1 個になる。

### 7.3 既存 rule の拡張は新 rule ではない

`clippy::collapsible_match` は jals の `collapsible-if` を `instanceof` パターンまで広げる話で、
`clippy::const_is_empty` は `constant-condition` の定数畳み込みを広げる話である。この 2 行は N のまま
（挙動が未実装だから）だが、`note` 列に `既存 rule の拡張` と書いてある。`tests/inventory.rs` は
「N 行の写り先が既に実装済みなら、note がそう言っていること」を強制する — さもないと rule が実装された
あと、それを未実装だと主張する行が黙って残る。

### 7.4 section ごとの内訳

286 rule の section 別内訳と、rule ごとの source lint 対応は `jals-lint/README.md` の
「The 286 planned rules, by section」にある。本書はどれが N かを決め、README がそれをどう並べるかを決める。

---

## 8. 台帳をどう保つか

**この文書の主張のうち、検証できるものは全て `jals-lint/tests/inventory.rs` にある。**

| 主張 | テスト |
|---|---|
| §0 の母数 244 / 815 | `every_lint_the_toolchain_ships_has_a_row` |
| §2 のバケツ表が 1,059 に一致する | `the_buckets_sum_to_the_source_set` |
| M 行の写り先が実際に実装されている | `every_mapped_row_names_a_rule_that_exists` |
| N 行の写り先が未実装（か、拡張だと明示している） | `a_planned_row_names_a_rule_that_does_not_exist_yet` |
| D 行に理由がある / M・N 以外は rule を名指さない | `only_mapped_and_planned_rows_name_a_rule_and_only_rejected_rows_give_a_reason` |
| 祖先を持つ実装済み rule が台帳から辿れる | `every_implemented_rule_that_ports_one_is_reachable_from_the_ledger` |

**検証できないもの**もはっきりさせておく。「`clippy::manual_strip` に Java の綴りが無い」は算術ではなく
主張であり、テストが settle できるものではない。だから理由は TSV の `note` 列と §6 に書いてあり、
テストが強制するのは「理由が書いてあること」までである。台帳の値は**議論が合計に合っていること**にあり、
一つ一つの判断が正しいことを機械が保証することにはない。

**toolchain を上げたら、テストが落ちる。** それが意図した挙動である。lint が増えた分だけ台帳に行が要り、
バケツの数が変わる。§0 の抽出コマンドをもう一度回して差分を分類し、§2 の表と `tests/inventory.rs` の
期待値を同時に更新する — 台帳が「全部を覆っている」と言えるのは、その手続きを踏んでいる間だけである。
