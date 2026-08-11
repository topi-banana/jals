# rustfmt option → jals rule 写像

> **この文書の位置づけ**: [`MAPPING.md`](MAPPING.md) が「4 つの **Java** フォーマッタの rule を
> jals のどの rule に写すか」の台帳であるのに対し、本書は **rustfmt の全 option** について
> 「jals のどの rule になるのか／ならないのか、ならないなら何故か」を 1 行 1 option で確定した
> 台帳である。
>
> **なぜ MAPPING.md に列を足さないのか。** MAPPING.md §2 の採否基準は「**到達可能な 2 つの
> ターゲット設定**がその挙動で食い違うか」で、ここでいうターゲットとは Eclipse / IntelliJ /
> google-java-format / Palantir / Spotless のことである。rustfmt は jals が近似しようとする
> ターゲットではない — Java を整形しないので、比較できる出力が存在しない。§5 の表に rustfmt 列を
> 足すことは、その前提を黙って覆すことになる。だから別ファイルにし、相互参照だけを張る。

---

## 0. 抽出元の固定

写像表は一次情報から機械的に抽出したものであり、抽出点を固定しておかないと再検証できない。

| 項目 | 値 |
|---|---|
| リポジトリ | `rust-lang/rustfmt` |
| commit | `936b23ec931c87378eb4c14ead576ab9ab98dd8c`（2026-08-11、`main` = `master`） |
| option 一覧 | `src/config/mod.rs` の `create_config!` — **90 option**（宣言順・stable/unstable 含む） |
| 既定値・値域 | `Configurations.md` — 内部専用の 6 個（`width_heuristics` / `file_lines` / `emit_mode` / `make_backup` / `print_misformatted_file_names` / `verbose`）は未掲載なので 84 見出し |
| jals 側 | `jals_config::fmt::Config` — 8 節 **196 key**（`jals-fmt/tests/coverage.rs` の `the_schema_is_the_documented_size` が固定） |

**数え方**: 母数は `create_config!` の 90。非推奨 alias（`fn_args_layout` / `merge_imports`）も
1 行として数える。§2 のバケツの合計は 90 に一致する（一致しない表は完全性の台帳として無価値）。

---

## 1. 前史 — 旧 rule set は rustfmt 移植だった

`jals_config::fmt` は formatter 本体より前に書かれ、**rustfmt の option をそのまま Java に移植**した
ものだった。MAPPING.md §1 はそれを 4 つの問題（P1 語彙が Java のものでない／P2 冗長／P3 中核 3 族の
欠落／P4 doc が消えたコードの化石）として記録し、§4.1 で 7 個を「rustfmt 固有」として削除したうえで、
ベンダー観測に基づく現在の rule set に置き換えている。

したがって本書の写像は「rustfmt を捨てた」記録ではない。捨てられたのは**出自ではなく機構**である
（§4）。現に `[literals]` の 3 key は doc comment で `hex_literal_case` /
`float_literal_trailing_zero` を rustfmt 由来と明記しており、本作業で足した 6 key も同じ位置に立つ
（§6）。

**残っていた化石を 1 つ片付けた**: `jals-config/src/fmt/options.rs` は `mod options;` を持たず
参照も皆無の dead code で、2 値の `IndentStyle`（`Mixed` が無い）、`rename_all = "lowercase"`
（生きている enum は全て kebab-case）、rustfmt `newline_style` を引く doc comment を持っていた。
P4 が言う「消えたコードの化石」そのものなので削除した。

---

## 2. 分類

全 90 option を 6 バケツに割る。**未実装は N だけ**で、他の 5 つは答えが出ている。

| 記号 | 意味 | 数 |
|---|---|---|
| **M** | 既存 jals rule に写る | 28 |
| **X** | Rust 固有（**機構レベル**）— 構文ごとの部分幅しきい値 / ヒューリスティック | 10 |
| **R** | Rust 固有（**構文名指し**） | 23 |
| **D** | 意図的に不採用（典拠あり） | 6 |
| **C** | formatter rule ではない（driver / CLI） | 16 |
| **N** | 移植可能で、本作業で**実装した** | 7 |
| | **合計** | **90** |

**X と R の線引き**。「rust 固有」を機構レベルで取る、というのが本作業の前提である。すなわち
MAPPING.md §4.1 が既に「rustfmt 固有」と呼んでいる定義をそのまま使い、Rust 構文を名指しする
option（R）に加えて、rustfmt 特有の**部分幅しきい値**という機構（X）も Rust 固有として除外し、
対応する `WrapPolicy` 語彙を併記する。理由は §4。

---

## 3. 全 90 option

既定値は `Configurations.md` のもの。「—」は該当なし。

### 3.1 M — 既存 jals rule に写る（28）

| rustfmt | 既定 | jals |
|---|---|---|
| `max_width` | 100 | `layout.max-width`（100） |
| `hard_tabs` | false | `layout.indent-style`（jals は `space`/`tab`/`mixed` の 3 値に一般化） |
| `tab_spaces` | 4 | `layout.indent-width`（4）＋ `layout.tab-width`（4）。jals は 2 分割 — `mixed` では 1 レベルが indent-width 列、それを tab-width 幅のタブで描く |
| `newline_style` | Auto | `layout.line-ending`。`Auto`/`Native`/`Unix`/`Windows` ↔ `auto`/`native`/`lf`/`crlf` と 1:1 |
| `wrap_comments` | false | `comments.format-line` / `format-block` / `format-javadoc`（3 分割）。3 ベンダー共通の分け方 |
| `format_code_in_doc_comments` | false | `comments.format-source-in-comments`（Eclipse `comment.format_source_code`） |
| `comment_width` | 80 | `comments.width`（80） |
| `format_strings` | false | `wrapping.reflow-long-strings`（GJF `StringWrapper`） |
| `hex_literal_case` | Preserve | `literals.hex-case`。**doc に rustfmt 由来と明記済み** |
| `float_literal_trailing_zero` | Preserve | `literals.float-trailing-zero`。`IfNoPostfix` は Java で `1.f` も `1.0f` も合法なので意味が空 → 意図的に欠番 |
| `empty_item_single_line` | true | `braces.keep-*-on-one-line = if-empty`（8 key） |
| `fn_single_line` | false | `braces.keep-method-body-on-one-line` |
| `force_multiline_blocks` | false | `braces.keep-*-on-one-line = never` |
| `reorder_imports` | true | `imports.order = "sort"` |
| `group_imports` | Preserve | `imports.order = "group"` ＋ `imports.groups` |
| `type_punctuation_density` | Wide | `spacing.around-type-bounds`（Java の bound は `&`） |
| `space_before_colon` | false | `spacing.before-{ternary,foreach,label,case,assert}-colon`（5 分割 = §4.1 P2 の解消） |
| `space_after_colon` | true | `spacing.after-*-colon`（同 5 分割） |
| `binop_separator` | Front | `wrapping.before-binary-operator`（Front=true）。jals は演算子種別ごとに 6 分割（`-ternary-operator` / `-assignment-operator` / `-method-chain-dot` / `-comma` / `-assert-colon`） |
| `short_array_element_width_threshold` | 10 | `wrapping.fill-item-width`（既定 0、GJF は 10）。**幅族の唯一の生き残り**（§4） |
| `match_arm_indent` | true | `layout.indent-switch-labels` ＋ `layout.indent-switch-case-body` |
| `fn_params_layout` | Tall | `wrapping.method-parameters`。`Compressed`/`Tall`/`Vertical` → `if-long`/`if-long-per-item`/`always-per-item` |
| `fn_args_layout` | Tall | 同上（非推奨 alias） |
| `brace_style` | SameLineWhere | `braces.type-declaration` / `braces.method-declaration`（`BraceStyle` 5 値）。`SameLineWhere` の `where` 判定は Rust 固有なので `same-line` に潰れる |
| `control_brace_style` | AlwaysSameLine | `braces.block` ＋ `braces.{else,while,catch,finally}-on-new-line`。`ClosingNextLine` = `else-on-new-line = true` |
| `blank_lines_upper_bound` | 1 | `blank-lines.max-in-code`(1) / `max-in-declarations` / `max-before-closing-brace` / `max-after-doc-comment` |
| `blank_lines_lower_bound` | 0 | `[blank-lines]` の enforce 系 18 key |
| `inline_attribute_width` | 0 | `wrapping.inline-argumentless-annotations`。**幅ではなく 3 値 enum** で表す（= X の機構をそのまま置き換えた例） |

### 3.2 X — Rust 固有（機構レベル）（10）

いずれも「1 つの列上限 ＋ 構文ごとの `WrapPolicy`」で表す。§4 を参照。

| rustfmt | 既定 | 対応する jals の考え方 |
|---|---|---|
| `use_small_heuristics` | Default | 幅しきい値族の一括プリセット。jals に幅族が無いので対応物も無い |
| `width_heuristics` | — | 同上（内部 option） |
| `fn_call_width` | 60 | `wrapping.call-arguments`（`WrapPolicy`） |
| `attr_fn_like_width` | 70 | `wrapping.annotation-arguments` |
| `array_width` | 60 | `wrapping.array-initializer` |
| `chain_width` | 60 | `wrapping.method-chain` |
| `single_line_if_else_max_width` | 50 | `braces.keep-control-statement-on-one-line`（bool）／`wrapping.ternary` |
| `doc_comment_code_block_small_heuristics` | Default | `use_small_heuristics` の comment 内版 |
| `combine_control_expr` | true | Rust の制御構文は**式**で、引数に置ける。Java の `if`/`for` は文なので該当構文が無い |
| `overflow_delimited_expr` | false | MAPPING.md §4.1 が明示的に切った行。Eclipse/IntelliJ に該当概念が無く、GJF の該当挙動は固定 |

### 3.3 R — Rust 固有（構文名指し）（23）

| rustfmt | Java に無いもの |
|---|---|
| `struct_lit_width` / `struct_variant_width` / `struct_lit_single_line` | struct リテラル・struct variant |
| `single_line_let_else_max_width` | `let ... else` |
| `where_single_line` | `where` 節（最も近いのは `throws`、= `wrapping.throws-list`） |
| `normalize_doc_attributes` | `#[doc]` |
| `format_macro_matchers` / `format_macro_bodies` / `skip_macro_invocations` | 宣言的マクロ |
| `spaces_around_ranges` | `..` / `..=` |
| `match_arm_leading_pipes` | `match` の先頭 `|`（Java の `case A, B ->` はカンマ区切りで先頭記号が無い） |
| `match_block_trailing_comma` | ブロック arm の末尾カンマ |
| `trailing_semicolon` | `break`/`continue`/`return` のセミコロン省略（Java では必須） |
| `reorder_modules` | `mod` 文 |
| `merge_derives` | `#[derive(...)]` |
| `use_try_shorthand` | `try!` / `?` |
| `use_field_init_shorthand` | フィールド初期化の短縮記法 |
| `force_explicit_abi` | `extern` の ABI |
| `condense_wildcard_suffixes` | タプルパターンの `..` |
| `edition` / `style_edition` / `version` | Rust のエディション・Style Guide 版・rustfmt 自身の版 |
| `skip_children` | 行外モジュール |

### 3.4 D — 意図的に不採用（典拠あり）（6）

**欠落ではない。** 各行は「調べた結果、置かないと決めた」記録である。

| rustfmt | 既定 | 典拠 |
|---|---|---|
| `indent_style` | Block | `Visual` は**列揃え**で、`DESIGN.md` §18.2 の恒久差分 **D1**（単一エンジンでは再現しないと確定）。`Block` は jals の固定挙動であって切り替え option が存在しない |
| `imports_indent` | Block | 同上。方言の grouped import に対しても `Visual` は取らない |
| `struct_field_align_threshold` | 0 | 列揃え（D1）。Eclipse `align_type_members_on_columns` / IntelliJ `ALIGN_CONSECUTIVE_*` と同族で、MAPPING.md §7 が非写像と記録済み |
| `enum_discrim_align_threshold` | 0 | 同上 |
| `trailing_comma` | Vertical | MAPPING.md §4.1 — 4 ベンダー全員が「原文保存」で一致するので、option ではなく**不変条件**（有意トークン多重集合の保存）にした。唯一の例外は方言 grouped import の末尾カンマで、これは `token_license` の無条件 row |
| `reorder_impl_items` | false | 型メンバの並べ替え。IntelliJ `FORCE_REARRANGE_MODE` を非写像と決めた MAPPING.md §7 の行と同族 |

### 3.5 C — formatter rule ではない（16、対象外）

`jals-cli` / manifest の領分。`jals_config::fmt::Config` に入れると CLAUDE.md のクレート境界
（「`jals-config` = pure schemas」）を破る。

| rustfmt | jals での居場所 |
|---|---|
| `color`, `verbose`, `emit_mode`, `make_backup`, `print_misformatted_file_names`, `file_lines` | `jals fmt` の CLI フラグ（`--check` / `--diff` など） |
| `ignore` | パス選択。`jals fmt <paths>` と `.gitignore` の領分 |
| `disable_all_formatting` | 全体無効化。jals は領域単位の `layout.formatter-tags` を持つ |
| `hide_parse_errors` / `show_parse_errors` | `FormatOutput::warnings` の提示方法（ホスト側） |
| `error_on_line_overflow` / `error_on_unformatted` | 診断ポリシー。jals は fail-safe（`FormatOutput::fell_back`）で別の答えを出している |
| `format_generated_files` / `generated_marker_line_search_limit` | 対象ファイル選択 |
| `required_version` / `unstable_features` | ツールチェーン宣言。jals では manifest 側の関心事 |

### 3.6 N — 実装した（7 option / 新規 6 key）

§6 に設計判断、以下は対応表のみ。

| rustfmt | 既定 | jals（新規） | 既定 |
|---|---|---|---|
| `imports_granularity` | Preserve | `[imports] granularity` | `preserve` |
| `merge_imports`（非推奨 alias） | false | 同上 | — |
| `imports_layout` | Mixed | `[wrapping] import-group`（`WrapPolicy`） | `never` |
| `normalize_comments` | false | `[comments] normalize-block-comments` | `false` |
| `remove_nested_parens` | true | `[wrapping] remove-nested-parens` | `false` |
| `doc_comment_code_block_width` | 100 | `[comments] code-block-width` | `0`（= `comments.width`） |
| `match_arm_blocks` | true | `[braces] force-switch-arm`（`ForceBraces`） | `never` |

---

## 4. なぜ「幅しきい値」機構は Java で成立しないのか

X バケツの 10 個は、rustfmt が持つ**構文ごとの部分幅しきい値**という機構に属する。
MAPPING.md §4.1 の P1 が既に結論を出している:

> `chain-width` / `fn-call-width` / `array-width` / `single-line-if-else-max-width` は rustfmt 固有の
> 「構文ごとの部分幅しきい値」で、**4 ベンダーのいずれも持たない**。Java の 3 エンジンはすべて
> 「単一の列上限 + 構文ごとの *wrap 方針 enum*」で折る（Eclipse `alignment_for_*` 53 個 /
> IntelliJ `*_WRAP` 26 個）。

徴候は importer 側に出ていた。GJF importer は `chain_width = max_width` のように**しきい値を
無効化する値を書き込む**しかなく、これは「その option が写像先として存在しない」ことの表れだった。

**ただし 1 つだけ生き残っている。** `short_array_element_width_threshold`（既定 10）は
`wrapping.fill-item-width` になっている。これが例外なのは、測っている対象が違うからである:

- `chain_width` などは**これから決めるレイアウト**の幅を測る（だから同じ入力でも設定次第で答えが変わる）
- `fill-item-width` は**著者が書いた項目の原文幅**を測る（だから入力の性質であって、レイアウトの関数ではない）

そして後者は GJF が `MAX_ITEM_LENGTH_FOR_FILLING = 10` として実際に持っている固定挙動である。
既定値まで rustfmt と一致するのは偶然ではなく、「短い項目は詰めてよいが、長いものが 1 つでも混ざれば
詰め方が恣意的になる」という同じ判断を両者が独立に下したためである。jals の既定は `0`（無効）で、
GJF プロファイルが `10` を書き込む。

`inline_attribute_width` も同じ置き換えを受けている。rustfmt は「注釈と宣言の合計幅がしきい値未満なら
同じ行」と幅で言うが、jals は `wrapping.inline-argumentless-annotations` という 3 値 enum
（`never` / `locals` / `declarations`）で言う。GJF の `fieldAnnotationDirection` が
「引数付き注釈が 1 つでもあれば縦、なければ横」という**構造の判定**であって幅の判定ではないからである。

---

## 5. 逆方向 — jals 196 対 rustfmt 90

写像は全単射ではない。rustfmt 側に対応が無い jals rule のほうがはるかに多く、その内訳が
「Java フォーマッタの語彙」そのものである。

| jals の節 | key 数 | rustfmt 側の対応 |
|---|---|---|
| `[layout]` | 16 | 6 個（`max_width`, `hard_tabs`, `tab_spaces`, `newline_style`, `match_arm_indent` ほか）。`formatter-tags` 3 個・`trim-trailing-whitespace`・`indent-empty-lines`・`label-indent`・`indent-type-members` は rustfmt に無い |
| `[blank-lines]` | 22 | 2 個（`blank_lines_upper_bound` / `_lower_bound`）。rustfmt は「上限と下限」しか持たず、jals は Eclipse/IntelliJ に倣って**位置ごとに** 20 個持つ |
| `[braces]` | 25 | 3 個（`brace_style`, `control_brace_style`, `match_arm_blocks`）。`BraceStyle` 5 値・`KeepOnOneLine` 5 値・`force-*` 5 個の大半は rustfmt に対応が無い |
| `[wrapping]` | 49 | 実質 3 個（`binop_separator`, `fn_params_layout`, `imports_layout`）＋ 幅族 5 個の置き換え先。per-construct の `WrapPolicy` 22 個・`ParenPositions` 6 個は Eclipse/IntelliJ 由来 |
| `[spacing]` | 49 | 3 個（`type_punctuation_density`, `space_before_colon`, `space_after_colon`）。残る 46 は Eclipse `insert_space_*` 219 / IntelliJ `SPACE_*` 45 の集約で、rustfmt には空白 option がほとんど無い |
| `[comments]` | 26 | 4 個（`wrap_comments`, `comment_width`, `format_code_in_doc_comments`, `doc_comment_code_block_width`, `normalize_comments`）。Javadoc の構造規則（`@param` 整列・`<p>` の扱い・HTML リスト・`{@code}` 分割）に rustfmt の対応物は無い |
| `[imports]` | 6 | 4 個（`reorder_imports`, `group_imports`, `imports_granularity`, `imports_layout`）。`reorder-modifiers` / `remove-unused` は GJF 由来 |
| `[literals]` | 3 | 2 個（`hex_literal_case`, `float_literal_trailing_zero`）。`suffix-case`（`123l` vs `123L`）は Java 固有 |

**要約**: rustfmt の 90 option のうち Java に写るのは 35（M 28 ＋ N 7）で、jals の 196 rule のうち
rustfmt に由来を持つのはその 35 に対応する部分だけである。残る大半は Eclipse 416 / IntelliJ 297 の
観測から来ている（MAPPING.md §3）。

---

## 6. N の 6 key — 設計判断

7 つとも**どの Java ベンダーも produce しない jals-native rule** で、既定値は `preserve` / `never` /
`false` / `0`。これは `[literals]` が既に占めている位置（MAPPING.md §4.3 の脚注、§6 のテスト 3 が
「どの importer からも動かない」と明示的に記録している唯一の族）と同格である。

### 6.1 `[imports] granularity` ← `imports_granularity`

方言の grouped import（`import java.util.{HashMap, List};`）に対する merge / split。

**値域を 3 に潰した理由。** rustfmt の `Crate` と `Module` は Java では同一である — Java に crate は
無く、module ≒ package なので、両者は `package` 1 値になる。`One`（異なる crate を 1 つの `use` に
まとめる）は Java に構文が無い: グループは 1 つの package prefix を共有するので、2 つの package が
1 宣言に同居できない。よって `preserve` / `package` / `item` の 3 値。

**`package` は方言が有効なプロジェクトでしか動かない。** grouped import は
`[package] features = ["grouped-imports"]` で有効になる方言機能で、`VanillaFrontend` は脱糖しない。
パーサは lossless なのでどのプロジェクトでも grouped import を受理し、fail-safe も通るので、
**方言 off のプロジェクトで merge すると `jals build` だけが壊れる**。formatter はこれを自力で
検出できない。

そこで `FormatOutput::format_source` は `FeatureSet` を受け取るようになった:

```rust
pub async fn format_source(src: &str, config: &Config, features: FeatureSet) -> Self
```

`Style::reify` が `granularity == Package && !features.permits(GroupedImports)` を `Preserve` に丸め、
`Warning` を出す。これは `DESIGN.md` §17 の丸め機構そのものだが、読んでいる事実が「入力の改行」では
なく「プロジェクトの feature set」である点だけが違う。分解方向（`item`）は方言構文を**減らす**だけ
なので丸めない。

ホスト側は各々が既に持っている経路から feature set を取る: `jals-cli` は `jals.toml` を上方探索する
`HostFeatures`（`jalsfmt.toml` を探す `HostConfigs` の兄弟）、`jals-lsp` と `jals-playground` は
`jals_editor::Workspace::feature_set()`。

**merge は隣接するものだけを結合する。** 非隣接の宣言を隣接させるのは `[imports] order` の仕事で、
ここでやると「原文保存」を頼んだブロックを黙って並べ替えることになる。ワイルドカード
（`import a.*;`）と module import は merge しない。1 件だけの run はグループにしない
（`import a.B;` → `import a.{B};` は何も言わない中括弧を足すだけ）。

**属性付きの宣言は merge も split もしない。** `#[cfg(feature = "x")] import a.{B, C};` を割ると
2 件目以降が gate から外れ、逆に merge すると 1 件目の条件が他の member を覆う。どちらも
「ファイルが何にコンパイルされるか」を変える書き換えで、しかも fail-safe には見えない — 属性の
トークンは row が既に免除している `IMPORT_DECL` の内側にあり、`ImportedNames` は型を答えるので
条件については何も言わない。

**fail-safe。** merge / split は有意トークン多重集合を構造的に変える（split は `import` / prefix /
`;` を増やし、`{}` `,` を減らす）。`Effect::RemovesSubtrees` は**減少しか**許可しないので使えず、
`Removes` / `Redistributes` は `COMMA` を名指しすることになり、方言の末尾カンマ row と同一の
specificity 段に入って互いを隠す（`equal_specificity_rows_cannot_mask_each_other` が禁じる）。
そこでノード単位版の `Redistributes` として `Effect::Recuts { kind, content }` を新設し、専用の
specificity 段（`Removes` の下、`RemovesSubtrees` の上）に置いた。守るのは
`Content::ImportedNames` — 宣言が名指しする型の完全修飾名の多重集合で、merge も split もこれを
厳密に保つ。**部分集合**判定なのは、この段が `remove-unused` の row より上にあり、両方 on のとき
import ブロック全体をこの row が答えるため（`remove-unused` は名前を消すのが仕事）。閉じているのは
「作り出していない」側 — prefix の再結合を誤れば入力に無い型の import が現れる。

### 6.2 `[wrapping] import-group` ← `imports_layout`

grouped import のメンバ一覧の折り返し。`WrapPolicy` の 4 値がそのまま
`Horizontal`/`Mixed`/`HorizontalVertical`/`Vertical` に対応する。既定 `never` は方言の canonical 形。

**`visit/dialect.rs` の前提を書き換えた。** 同ファイルは末尾カンマを無条件に落とす根拠として
「a group is always laid out flat, so there is no vertical form for a trailing comma to serve」と
書いていた。縦形が入るとこの根拠は成立しない。**drop は無条件のまま**にし、根拠を「方言の
canonical form に trailing comma は存在しない」に書き換えた。gate 化できないのは、
`the_default_config_licenses_exactly_the_unconditional_rows` が「gate 付き row は既定で必ず off」を
要求するため — 既定 on の gate 付き row は作れない。

### 6.3 `[comments] normalize-block-comments` ← `normalize_comments`

`/* … */` → `//`。rustfmt の "where possible" を明示的な述語にした: **原文で単独行に置かれた**
block comment だけを変換する。`foo(/* x */ y)` や行末の `/* why */` を変換すると、以降のコードが
次行に押し出される — 何も頼まれていないレイアウト変更であり、コメント内部には有意トークンが無いので
fail-safe には見えない。

述語は出力ではなく**入力**の事実である必要があるので、`CommentMap::build` で
`Comment::alone_on_line` として記録する（`own_line` は「出力が単独行に置くか」で、式中の `/* x */`
でも true になりうる）。この区別のために `CommentFormatter::render` の 3 つの位置フラグを
`Placement` 構造体にまとめた。

コメントは trivia なので `TokenBudget` の対象外（`OPERATIONS` の row は不要）だが、
`invariants.rs::no_comment_is_ever_dropped` の緩和条件には加えた — 複数行 block が N 個の line
comment になるのでコメントの**個数**が変わる。

変換後のコメントは `//` で終わるので、続くトークンは必ず次行に送らねばならない。既存の判定は
`Comment::is_line()`（= **原文の**種別）を読んでいて、変換後は「block だから改行不要」と答えて
しまう。判定を `Ctx::emit_comment` に移し、**実際に出力したテキスト**の最終行が `//` で始まるかを
見るようにした — 答えが 1 つになる。読み違えたときの症状は新規 syntax error → ファイル全体が
未整形で返る、である。

### 6.4 `[wrapping] remove-nested-parens` ← `remove_nested_parens`

`((x + y))` → `(x + y)`。**既定は `false`**（rustfmt は `true`）。トークンを消すのは Java
フォーマッタが頼まれずにやることではなく、4 ベンダー全員が冗長な括弧を原文どおり残す。

述語は「`PAREN_EXPR` の唯一の子が `PAREN_EXPR`」で、外側の対を落とす。cast の括弧・呼び出しの
引数リスト・制御文の条件は `PAREN_EXPR` の子ではないので候補にならない。`(((x)))` が `(x)` になるのは
数えているからではなく、各冗長対に順に到達するからである。

`License::is_redundant_paren` が pass と check の共有述語（`token_license` の
one-predicate-two-callers 規則）。row は `Removes { kinds: [LPAREN, RPAREN], site: RedundantParen }`。

### 6.5 `[comments] code-block-width` ← `doc_comment_code_block_width`

**§2 の採否基準を満たさない**ことは明示しておく: Eclipse は `comment.line_length` を prose と共用し、
snippet 専用の幅を持たない。既定 `0` =「`comments.width` に従う」とすることで、到達可能な既存設定は
どれも動かない。

守る対象も rustfmt より狭い。jals は snippet を**再整形しない**（字下げの正規化だけ）ので、
「再字下げした結果がこの幅を超えるなら、著者が書いた字下げのまま残す」という予算として働く。
領域単位で全部やるか全部やらないかを決める — 半分だけ再字下げされた snippet はどちらより情報が少ない。

### 6.6 `[braces] force-switch-arm` ← `match_arm_blocks`

`case A -> run();` → `case A -> { run(); }`。**switch 文の arm のみ**。

switch *式* の arm は値を作らねばならないので、ブロック化には `case A -> { yield f(); }` と
`yield` の挿入が要る。これはレイアウトではなく**意味の書き換え**なので、switch 式の arm には一切
触れない。既にブロックか `throw` の arm も対象外。

既存の `braces.force-{if,for,while,do-while}` は IntelliJ `*_BRACE_FORCE` 4 個が典拠だが、arrow
`case` に対応する vendor option は無いので、これは jals-native として記録する。`token_license` では
既存の `force-*` の `Inserts` row に合流する（同じ効果、同じ kind）。

---

## 7. 腐らせないために（提案 — **本作業では実装しない**）

MAPPING.md §6 は「1 つも欠落しない」を宣言ではなく**テスト**で守っている。Eclipse 416 / IntelliJ 297
の `inventory.tsv` を各 importer のカバレッジテストが読み、目録の各行がモデルに存在することを表明する。

同じ仕組みは rustfmt にも適用できる:

1. `jals-fmt/rustfmt-inventory.tsv` — `src/config/mod.rs` から機械抽出した 90 行（option 名・既定値・
   stable/unstable・分類記号）
2. `the_rustfmt_inventory_is_the_documented_size` — 行数を 90 に固定
3. `every_rustfmt_option_is_classified` — 各行の分類記号が M/X/R/D/C/N のいずれかであり、
   M と N の行は §3 の jals key 名が実在の schema leaf を指すことを表明

これを入れれば「rustfmt が option を足したら落ちる」台帳になる。ただし本作業の依頼範囲は
「写像を明示し、未実装を洗い出し、実装し、文書化する」であって、追随機構の構築ではないので
**提案として記すに留める**。

---

## 8. 相互参照

- [`MAPPING.md`](MAPPING.md) — ベンダー rule → jals rule の台帳。採否基準（§2）、切った/一般化した/
  足した rule（§4）、写像表（§5）、テスト（§6）、意図的に写さないもの（§7）
- [`DESIGN.md`](DESIGN.md) — §8 の 4 つの seam、§17 の丸め、§18.2 の恒久差分、§20 の
  `OPERATIONS` 表
- `jals-fmt/tests/coverage.rs` — 196 rule が実際に formatter に届くことのゲート
- `jals-fmt/src/passes/token_license.rs` — 有意トークンを変える全操作の表（本作業で 8 → 10 行）
