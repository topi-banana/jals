# jals-fmt config: ベンダー rule 観測 → jals rule 写像

> **この文書の位置づけ**: `DESIGN.md` §15（`jalsfmt.toml` 自動生成）と付録 A（config ファイル形式）を
> 受けて、**「どのベンダー rule を観測し、どの jals rule に写すか」**を決めた台帳。
> `jals_config::fmt` の rule set と `jals_fmt::import` の native モデルは、どちらも本書の表を実装したもの。
>
> 一次情報から機械抽出した**完全な option 目録**は、本書の付随データとしてリポジトリに入っている:
> - `jals-fmt/src/import/eclipse/inventory.tsv` — 416 id
> - `jals-fmt/src/import/intellij/inventory.tsv` — 297 setting
>
> この 2 ファイルは飾りではなく、各 importer の**カバレッジテストが読む入力**である（§6）。

---

## 1. なぜ作り直すのか — 旧 rule set の 4 つの問題

`jals-config/src/fmt` は formatter 本体より前に書かれ、**rustfmt の option をそのまま Java に移植**した
ものだった。ベンダー観測を経ていないため、次の 4 つが同時に起きていた。

**P1. 語彙が Java フォーマッタのものではない。**
`chain-width` / `fn-call-width` / `array-width` / `single-line-if-else-max-width` は rustfmt 固有の
「構文ごとの部分幅しきい値」で、**4 ベンダーのいずれも持たない**。Java の 3 エンジンはすべて
「単一の列上限 + 構文ごとの *wrap 方針 enum*」で折る（Eclipse `alignment_for_*` 53 個 / IntelliJ
`*_WRAP` 26 個）。だから GJF importer は `chain_width = max_width` のように**しきい値を無効化する値を
書き込む**しかなく、これは「その option が写像先として存在しない」ことの徴候だった。

**P2. 冗長 — 同じ事実を複数の option が別々に表している。**
- `empty-item-single-line` / `fn-single-line` / `force-multiline-blocks` の 3 つは、Eclipse の
  `keep_*_on_one_line`（1 個の 5 値 enum）1 つで表せる事実を 3 つの bool に割り、しかも
  `force-multiline-blocks` が他 2 つを上書きするという**優先順位を doc でしか表現できない**状態だった。
- `reorder-imports` と `group-imports` は「後者が前者を含意し上書きする」関係で、実質 3 値 enum。
- `space-before-colon` と `space-around-operator-colon` は**加算的**（どちらかが on なら空白）という
  独自規則で、ベンダーのどれにも対応せず、`for`/ternary/`assert`/label/`case` の 5 文脈を 2 つの
  bool に潰していた。Eclipse は同じ 5 文脈を独立した `insert_space_*_colon_in_*` として持つ。

**P3. 欠落 — Java フォーマッタの中核 3 族がまるごと無い。**
`spacing`（Eclipse 219 / IntelliJ 45）・`blank-lines`（Eclipse 15 / IntelliJ 17）・per-construct
`wrapping`（Eclipse 53 / IntelliJ 26）が、jals 側には**colon 空白 2 個と `max-blank-lines` 1 個しか
存在しなかった**。importer が「著しく限定された rule しか実装していない」ように見えていた主因はこれで、
importer の手抜きではなく**写像先の不在**だった。

**P4. doc が実在しない実装を説明している。**
option の doc comment が、from-scratch rewrite で削除済みの formatter 実装（`lower_braced`、
「the prior behavior」等）を詳細に記述していた。仕様ではなく**消えたコードの化石**になっていた。

---

## 2. 判断基準 — 何を残し、何を切り、何を足すか

rule 単位で次の 1 行を適用する。

> **jals rule を置く ⇔ 到達可能な 2 つのターゲット設定が、その挙動で食い違う。**
> 切る ⇔ 全ターゲットが一致する（= option ではなく formatter の固定挙動にする）か、
> どのターゲットもその値を作れない（= 投機的）。

「多くのベンダーが持っている option を採る」ではないことが重要。`tabular-array-initializers` /
`case-labels-wrap` / `paren-positions = common-lines` は **GJF が固定挙動として持ち Eclipse/IntelliJ が
別挙動を持つ**から必要になる。逆に `trailing-comma` は 4 ベンダー全員が「原文保存」で一致するので、
option ではなく不変条件（有意トークン列保存）そのものにする。

この基準がそのまま §4 の切る/足す一覧と、各 rule の存在理由になっている。

---

## 3. 二層構造 — 完全性の基準は層ごとに違う

`DESIGN.md` §11 結論 5 は「統一スタイル言語は不可能、生成 toml は**エンジン多重化器**」と結論し、
§15 は engine 固有 option の**透過**を規定している。ブラッシュアップはこの結論に従い、
**2 つの成果物に別々の完全性基準**を置く。

| 層 | 成果物 | 完全性の基準 | 規模 |
|---|---|---|---|
| **native モデル** | `jals_fmt::import::{eclipse, intellij, gjf, palantir, spotless}` | **全数**。ベンダー option は 1 つも落とさない。jals に写像先が無い option も**型付きで保持**する | Eclipse 416 / IntelliJ 297 |
| **共通語彙** | `jals_config::fmt::Config` | **選別**。§2 の基準を満たすものだけ。union ではない | 8 節・174 |

「jals-config でキャプチャされない rule が構造化されていない」という問題への答えがこの表である。
**捨てるのではなく、native モデル側に型付きで残す。** 未写像の option は
- serde が読み、型が付き、`Debug`/`PartialEq` が効き、
- 将来 `DESIGN.md` §14 の `LayoutEngine`（Eclipse/IntelliJ 互換 engine）を移植したときに**そのまま
  engine のオプションになる**。

写像しないことと、モデル化しないことは別である。

---

## 4. jals rule set の改訂（§2 の基準の適用結果）

### 4.1 切る（7）

| 旧 rule | 理由 |
|---|---|
| `chain-width` / `fn-call-width` / `array-width` / `single-line-if-else-max-width` | P1。どのベンダーも持たない rustfmt 固有の部分幅。`[wrapping]` の per-construct `WrapPolicy` が代替 |
| `trailing-comma` | 4 者すべて原文保存で一致 → option ではなく不変条件 |
| `binop-layout` | `WrapPolicy` の `if-long`（fill）/ `if-long-per-item` に吸収 |
| `fn-params-layout` | 同上（`wrapping.method-parameters`）。`Tall`/`Compressed`/`Vertical` は `WrapPolicy` の 3 値と 1:1 |
| `overflow-delimited-expr` | rustfmt 固有。Eclipse/IntelliJ に該当概念が無く、GJF の該当挙動は固定 |
| `empty-item-single-line` / `fn-single-line` / `force-multiline-blocks` | P2。`braces.keep-*-on-one-line`（5 値 `KeepOnOneLine`）に統合 |
| `space-before-colon` / `space-after-colon` / `space-around-operator-colon` | P2。文脈別 10 option（ternary/foreach/label/case/assert × before/after）に分解 |

### 4.2 一般化する（意味は残すが表現を変える）

| 旧 | 新 | 変えた理由 |
|---|---|---|
| `brace-style` / `control-brace-style`（2 値 × 2） | `[braces]` の 6 個 × `BraceStyle`（5 値） | Eclipse 15 / IntelliJ 4 の brace position。`next_line_shifted`(Eclipse) / `whitesmiths`・`gnu`(IntelliJ) / `next_line_on_wrap` が 2 値に潰れていた |
| `control-brace-style` の `} else` 側 | `else/while/catch/finally-on-new-line` の 4 bool | 開き brace 位置と継続キーワード位置は独立した設定（IntelliJ は 4 個の別 option） |
| `annotation-placement`（2 値） | `wrapping.{type,method,field,parameter,variable}-annotations` × `WrapPolicy` | IntelliJ は宣言種ごとに 5 個。GJF は field だけ挙動が違う |
| `closing-paren`（2 値） | `wrapping.paren-*` 6 個 × `ParenPositions`（5 値） | Eclipse `parentheses_positions_in_*` の語彙。lparen 側も表現できるようになる |
| `blank-line-at-block-start`（bool） | `blank-lines.at-block-start`（数） | Eclipse/IntelliJ とも本数指定 |
| `max-blank-lines` | `blank-lines.max-in-code` ほか 20 個の族 | P3 |
| `reorder-imports` + `group-imports` | `imports.order`（3 値 enum） | P2 |
| `wrap-comments` + `comment-width` | `[comments]` 16 個 | P3。行/block/Javadoc を独立に制御するのが 3 ベンダー共通 |

### 4.3 足す（族単位）

| 節 | 数 | 主な典拠 |
|---|---|---|
| `[layout]` | 16 | `tab-width`（Eclipse `tabulation.size` と `indentation.size` の分離 / IntelliJ `TAB_SIZE`）、`trim-trailing-whitespace`（Spotless / EditorConfig）、`indent-empty-lines`、`indent-switch-labels`、formatter on/off タグ（Eclipse `disabling_tag` / IntelliJ `ij_formatter_off_tag` / Spotless `toggleOffOn`） |
| `[blank-lines]` | 20 | Eclipse `blank_lines_*` 15 + `number_of_blank_lines_*` / IntelliJ `BLANK_LINES_*` + `KEEP_BLANK_LINES_*` |
| `[braces]` | 24 | Eclipse `brace_position_for_*` 15 + `keep_*_on_one_line` 14 / IntelliJ `*_BRACE_STYLE` 4 + `*_BRACE_FORCE` 4 + `KEEP_SIMPLE_*` |
| `[wrapping]` | 42 | Eclipse `alignment_for_*` 53 + `wrap_before_*` 13 + `parentheses_positions_in_*` 11 / IntelliJ `*_WRAP` 26 + `ALIGN_MULTILINE_*` |
| `[spacing]` | 49 | Eclipse `insert_space_*` 219 / IntelliJ `SPACE_*` 45 |
| `[comments]` | 16 | Eclipse `comment.*` 25 / IntelliJ `JD_*` 20 + `WRAP_COMMENTS` |
| `[imports]` | 4 | IntelliJ `IMPORT_LAYOUT_TABLE` ほか / Spotless `importOrder` / GJF `ImportOrderer` |
| `[literals]` | 3 | jals 固有（どのベンダーも持たないが、3 者とも「書き換えない」で一致するため既定は `preserve`） |

`[literals]` は §2 の基準の例外に見えるが、既定値がすべて `preserve`＝全ベンダー一致の挙動であり、
非既定値は jals-native プロファイル専用である（§5.6 の表で 4 者すべてが「—」になっている唯一の族）。
この「どの importer からも動かない」性質は §6 のテスト 3 が明示的に表明している。

---

## 5. 写像表 — ベンダー rule → jals rule

`W` = `WrapPolicy`, `B` = `BraceStyle`, `K` = `KeepOnOneLine`, `P` = `ParenPositions`。
「—」は写像先を持たない（= native モデルにのみ存在する）。

### 5.1 `[layout]`

| jals rule | Eclipse | IntelliJ | GJF / Palantir | Spotless |
|---|---|---|---|---|
| `indent-style` | `tabulation.char` (tab/space/mixed) | `USE_TAB_CHARACTER` + `SMART_TABS` | space 固定 | `leadingTabsToSpaces` |
| `indent-width` | `tabulation.size` / mixed 時 `indentation.size` | `INDENT_SIZE` | 2 / AOSP 4 / Palantir 4 | — |
| `tab-width` | `tabulation.size` | `TAB_SIZE` | — | — |
| `continuation-indent` | `continuation_indentation` × indent-width | `CONTINUATION_INDENT_SIZE` | 4 / AOSP 8 / Palantir 8 | — |
| `max-width` | `lineSplit` | `RIGHT_MARGIN` | 100 / Palantir 120 | — |
| `line-ending` | — | `LINE_SEPARATOR` | LF | — |
| `insert-final-newline` | `insert_new_line_at_end_of_file_if_missing` | — | 常に true | `endWithNewline()` |
| `trim-trailing-whitespace` | — | — | 常に true | `trimTrailingWhitespace()` |
| `indent-empty-lines` | `indent_empty_lines` | `KEEP_INDENTS_ON_EMPTY_LINES` | false | — |
| `indent-switch-labels` | `indent_switchstatements_compare_to_switch` | `INDENT_CASE_FROM_SWITCH` | true | — |
| `indent-switch-case-body` | `indent_switchstatements_compare_to_cases` | `INDENT_BREAK_FROM_CASE` | true | — |
| `indent-type-members` | `indent_body_declarations_compare_to_type_header` | `DO_NOT_INDENT_TOP_LEVEL_CLASS_MEMBERS`（反転） | true | — |
| `label-indent` | — | `LABEL_INDENT_SIZE` / `LABEL_INDENT_ABSOLUTE` | 0 | — |
| `formatter-tags` / `-off-tag` / `-on-tag` | `use_on_off_tags` / `disabling_tag` / `enabling_tag` | `FORMATTER_TAGS_ENABLED` / `FORMATTER_OFF_TAG` / `FORMATTER_ON_TAG` | `// google-java-format:off` 相当なし | `toggleOffOn()` |

### 5.2 `[blank-lines]`

| jals rule | Eclipse | IntelliJ |
|---|---|---|
| `max-in-code` | `number_of_empty_lines_to_preserve` | `KEEP_BLANK_LINES_IN_CODE` |
| `max-in-declarations` | 同上 | `KEEP_BLANK_LINES_IN_DECLARATIONS` |
| `max-before-closing-brace` | `number_of_blank_lines_at_end_of_code_block` | `KEEP_BLANK_LINES_BEFORE_RBRACE` |
| `before-package` / `after-package` | `blank_lines_before_package` / `_after_package` | `BLANK_LINES_BEFORE_PACKAGE` / `_AFTER_PACKAGE` |
| `before-imports` / `after-imports` | `blank_lines_before_imports` / `_after_imports` | `BLANK_LINES_BEFORE_IMPORTS` / `_AFTER_IMPORTS` |
| `between-import-groups` | `blank_lines_between_import_groups` | （`IMPORT_LAYOUT_TABLE` の `<emptyLine/>`） |
| `around-type` | `blank_lines_between_type_declarations` | `BLANK_LINES_AROUND_CLASS` |
| `at-type-body-start` / `-end` | `blank_lines_before_first_class_body_declaration` / `_after_last_class_body_declaration` | `BLANK_LINES_AFTER_CLASS_HEADER` / `BLANK_LINES_BEFORE_CLASS_END` |
| `around-field` | `blank_lines_before_field` | `BLANK_LINES_AROUND_FIELD` |
| `around-method` | `blank_lines_before_method` | `BLANK_LINES_AROUND_METHOD` |
| `around-initializer` | `blank_lines_before_new_chunk` | `BLANK_LINES_AROUND_INITIALIZER` |
| `at-block-start` / `-end` | `number_of_blank_lines_at_beginning_of_code_block` / `_at_end_of_code_block` | — |
| `before-method-body` | `number_of_blank_lines_at_beginning_of_method_body` | `BLANK_LINES_BEFORE_METHOD_BODY` |
| `between-switch-groups` | `blank_lines_between_statement_group_in_switch` | `BLANK_LINES_BETWEEN_CASE_BLOCKS` |
| `around-field-in-interface` | `blank_lines_before_field`（共用） | `BLANK_LINES_AROUND_FIELD_IN_INTERFACE` |
| `around-method-in-interface` | `blank_lines_before_method`（共用） | `BLANK_LINES_AROUND_METHOD_IN_INTERFACE` |

### 5.3 `[braces]` — `BraceStyle` の語彙統合

`P-gen-5`（`DESIGN.md` §15）が警告する語彙衝突の実体。**この 1 表が両ベンダーを損なわずに載せる。**

| jals `BraceStyle` | Eclipse | IntelliJ (editorconfig / XML int) |
|---|---|---|
| `same-line` | `end_of_line` | `end_of_line` / `1` |
| `next-line` | `next_line` | `next_line` / `2` |
| `next-line-shifted` | `next_line_shifted` | `whitesmiths` / `3` |
| `next-line-shifted-braces` | — | `gnu` / `4` |
| `next-line-on-wrap` | `next_line_on_wrap` | `next_line_if_wrapped` / `5` |

`KeepOnOneLine` は Eclipse の 5 値をそのまま採る（IntelliJ の bool はその部分集合）:

| jals `KeepOnOneLine` | Eclipse `keep_*_on_one_line` | IntelliJ |
|---|---|---|
| `never` | `one_line_never` | `KEEP_SIMPLE_*_IN_ONE_LINE = false` |
| `if-empty` | `one_line_if_empty` | — |
| `if-single-item` | `one_line_if_single_item` | — |
| `always` | `one_line_always` | — |
| `preserve` | `one_line_preserve` | `KEEP_SIMPLE_*_IN_ONE_LINE = true`（入力空白依存） |

`preserve` が **`DESIGN.md` §17 の whitespace-retaining モードに落ちる唯一の値**であることに注意
（canonical モードでは `never` に丸められる）。

### 5.4 `[wrapping]` — `WrapPolicy` の語彙統合

**`P-gen-5` の 2 つ目の実体。** IntelliJ の token 名は反直感（`split_into_lines` = Wrap Always）で、
Eclipse は bit マスク。両者を 1 つの 4 値に載せる:

| jals `WrapPolicy` | Eclipse `alignment_for_*`（`SPLIT_MASK = 0x70`, `M_FORCE = 1`） | IntelliJ (`*_WRAP`) |
|---|---|---|
| `never` | `2147483647`（`Integer.MAX_VALUE` sentinel）/ split ビットなし | `off` / `0` |
| `if-long` | `M_COMPACT_SPLIT`(16) / `M_COMPACT_FIRST_BREAK_SPLIT`(32)、`M_FORCE` なし | `normal` / `1` |
| `if-long-per-item` | `M_ONE_PER_LINE_SPLIT`(48) / `M_NEXT_SHIFTED_SPLIT`(64) / `M_NEXT_PER_LINE_SPLIT`(80)、`M_FORCE` なし | `on_every_item`（Chop Down If Long）/ `4`・`5` |
| `always-per-item` | 上記 split ビット + `M_FORCE`(1) | `split_into_lines`（Wrap Always）/ `2` |

`ParenPositions` は Eclipse の語彙をそのまま採る。IntelliJ の 2 bool
（`*_LPAREN_ON_NEXT_LINE` / `*_RPAREN_ON_NEXT_LINE`）は次のように合流する:

| jals `ParenPositions` | Eclipse | IntelliJ (lparen, rparen) |
|---|---|---|
| `common-lines` | `common_lines` | (false, false) |
| `separate-lines-if-wrapped` | `separate_lines_if_wrapped` | — |
| `separate-lines-if-not-empty` | `separate_lines_if_not_empty` | — |
| `separate-lines` | `separate_lines` | (true, true) |
| `preserve` | `preserve_positions` | — |

（IntelliJ の (true,false) / (false,true) は非対称で jals に対応値が無い。`separate-lines` に寄せ、
非対称であったことは native モデル側に両 bool として残る。）

per-construct の対応（抜粋。全数は `inventory.tsv` と各 importer の `From` 実装）:

| jals `[wrapping]` | Eclipse | IntelliJ |
|---|---|---|
| `call-arguments` | `alignment_for_arguments_in_method_invocation` | `CALL_PARAMETERS_WRAP` |
| `method-parameters` | `alignment_for_parameters_in_method_declaration` | `METHOD_PARAMETERS_WRAP` |
| `record-components` | `alignment_for_record_components` | `RECORD_COMPONENTS_WRAP` |
| `resource-list` | `alignment_for_resources_in_try` | `RESOURCE_LIST_WRAP` |
| `throws-list` | `alignment_for_throws_clause_in_method_declaration` | `THROWS_LIST_WRAP` |
| `extends-list` | `alignment_for_superclass_in_type_declaration` | `EXTENDS_LIST_WRAP` |
| `enum-constants` | `alignment_for_enum_constants` | `ENUM_CONSTANTS_WRAP` |
| `array-initializer` | `alignment_for_expressions_in_array_initializer` | `ARRAY_INITIALIZER_WRAP` |
| `annotation-arguments` | `alignment_for_arguments_in_annotation` | `ANNOTATION_PARAMETER_WRAP` |
| `method-chain` | `alignment_for_selector_in_method_invocation` | `METHOD_CALL_CHAIN_WRAP` |
| `binary-operation` | `alignment_for_{additive,multiplicative,logical,relational,bitwise,shift,string_concatenation}_operator`（7 → 1、`additive` を代表に採り、残りは native 側に保持） | `BINARY_OPERATION_WRAP` |
| `ternary` | `alignment_for_conditional_expression` | `TERNARY_OPERATION_WRAP` |
| `assignment` | `alignment_for_assignment` | `ASSIGNMENT_WRAP` |
| `for-statement` | `alignment_for_expressions_in_for_loop_header` | `FOR_STATEMENT_WRAP` |
| `assert-statement` | `alignment_for_assertion_message` | `ASSERT_STATEMENT_WRAP` |
| `switch-expression` | `alignment_for_expressions_in_switch_case_with_arrow` | `SWITCH_EXPRESSIONS_WRAP` |
| `case-labels` | `alignment_for_expressions_in_switch_case_with_colon` | — (GJF 由来) |
| `multi-catch-types` | `alignment_for_union_type_in_multicatch` | `MULTI_CATCH_TYPES_WRAP` |
| `type-annotations` | `insert_new_line_after_annotation_on_type` | `CLASS_ANNOTATION_WRAP` |
| `method-annotations` | `insert_new_line_after_annotation_on_method` | `METHOD_ANNOTATION_WRAP` |
| `field-annotations` | `insert_new_line_after_annotation_on_field` | `FIELD_ANNOTATION_WRAP` |
| `parameter-annotations` | `insert_new_line_after_annotation_on_parameter` | `PARAMETER_ANNOTATION_WRAP` |
| `variable-annotations` | `insert_new_line_after_annotation_on_local_variable` | `VARIABLE_ANNOTATION_WRAP` |
| `before-binary-operator` | `wrap_before_additive_operator` ほか 7（旧 `wrap_before_binary_operator` は legacy fan-out 元） | `BINARY_OPERATION_SIGN_ON_NEXT_LINE` |
| `before-ternary-operator` | `wrap_before_conditional_operator` | `TERNARY_OPERATION_SIGNS_ON_NEXT_LINE` |
| `before-assignment-operator` | `wrap_before_assignment_operator` | `PLACE_ASSIGNMENT_SIGN_ON_NEXT_LINE` |
| `before-comma` | `wrap_before_comma_in_*`（複数 → 1） | — |
| `before-assert-colon` | `wrap_before_assertion_message_operator` | `ASSERT_STATEMENT_COLON_ON_NEXT_LINE` |
| `wrap-first-method-in-chain` | — | `WRAP_FIRST_METHOD_IN_CALL_CHAIN` |
| `join-wrapped-lines` | `join_wrapped_lines` | `KEEP_LINE_BREAKS`（反転） |
| `wrap-long-lines` | — | `WRAP_LONG_LINES` |

### 5.5 `[spacing]`

Eclipse の 219 `insert_space_*` は「文脈 × 前後」の直積で、jals は**トークン種別で束ねた 49**に落とす。
IntelliJ の `SPACE_*` 45 とはほぼ 1:1。代表例（全数は importer の `From` 実装）:

| jals `[spacing]` | Eclipse（代表 id、複数を集約） | IntelliJ |
|---|---|---|
| `around-assignment-operators` | `insert_space_before/after_assignment_operator` | `SPACE_AROUND_ASSIGNMENT_OPERATORS` |
| `around-additive-operators` | `insert_space_before/after_additive_operator` | `SPACE_AROUND_ADDITIVE_OPERATORS` |
| `around-lambda-arrow` | `insert_space_before/after_lambda_arrow` | `SPACE_AROUND_LAMBDA_ARROW` |
| `around-method-ref-double-colon` | `insert_space_before/after_colon_colon` | `SPACE_AROUND_METHOD_REF_DBL_COLON` |
| `before-comma` / `after-comma` | `insert_space_before/after_comma_in_*`（各 20 超） | `SPACE_BEFORE_COMMA` / `SPACE_AFTER_COMMA` |
| `before-method-call-parentheses` | `insert_space_before_opening_paren_in_method_invocation` | `SPACE_BEFORE_METHOD_CALL_PARENTHESES` |
| `before-keyword-parentheses` | `insert_space_before_opening_paren_in_{if,for,while,switch,catch,synchronized,try}` | `SPACE_BEFORE_{IF,FOR,…}_PARENTHESES` |
| `within-method-call-parentheses` | `insert_space_after_opening_paren_in_method_invocation` ほか | `SPACES_WITHIN_METHOD_CALL_PARENTHESES` |
| `before-left-brace` | `insert_space_before_opening_brace_in_*`（各種） | `SPACE_BEFORE_*_LBRACE`（12 個） |
| `before-ternary-colon` / `after-ternary-colon` | `insert_space_before/after_colon_in_conditional` | `SPACE_BEFORE_COLON` / `SPACE_AFTER_COLON` |
| `before-foreach-colon` / `after-foreach-colon` | `insert_space_before/after_colon_in_for` | `SPACE_BEFORE_COLON_IN_FOREACH` / — |
| `before-label-colon` / `after-label-colon` | `insert_space_before/after_colon_in_labeled_statement` | — |
| `before-case-colon` / `after-case-colon` | `insert_space_before/after_colon_in_case` | — |
| `before-assert-colon` / `after-assert-colon` | `insert_space_before/after_colon_in_assert` | — |
| `after-type-cast` | `insert_space_after_closing_paren_in_cast` | `SPACE_AFTER_TYPE_CAST` |
| `within-angle-brackets` | `insert_space_after_opening_angle_bracket_in_type_arguments` ほか | `SPACES_WITHIN_ANGLE_BRACKETS` |
| `around-type-bounds` | `insert_space_before/after_and_in_type_parameter` | `SPACES_AROUND_TYPE_BOUNDS_IN_TYPE_PARAMETERS` |
| `around-annotation-eq` | `insert_space_before/after_assignment_operator_in_annotation` | `SPACES_AROUND_ANNOTATION_EQ` |

**colon の 5 文脈が独立した 10 option になったことが、旧 `space-around-operator-colon` の加算規則
（P2）の解消**である。

### 5.6 `[comments]` / `[imports]` / `[literals]`

| jals rule | Eclipse | IntelliJ | GJF |
|---|---|---|---|
| `comments.format-line` | `comment.format_line_comments` | — | 常に true |
| `comments.format-block` | `comment.format_block_comments` | — | 常に true |
| `comments.format-javadoc` | `comment.format_javadoc_comments` | `ENABLE_JAVADOC_FORMATTING` | `--skip-javadoc-formatting` の否定 / Palantir `formatJavadoc`（既定 false） |
| `comments.width` | `comment.line_length` | （`RIGHT_MARGIN` 共用） | 100 |
| `comments.count-width-from-start` | `comment.count_line_length_from_starting_position` | — | — |
| `comments.format-header` | `comment.format_header` | — | — |
| `comments.format-html` | `comment.format_html` | — | true |
| `comments.format-source-in-comments` | `comment.format_source_code` | — | — |
| `comments.preserve-blank-lines` | `comment.clear_blank_lines_in_javadoc_comment`（反転） | `JD_KEEP_EMPTY_LINES` | true |
| `comments.blank-line-before-tags` | `comment.insert_new_line_before_root_tags` | `JD_ADD_BLANK_AFTER_DESCRIPTION` | true |
| `comments.align-tag-descriptions` | `comment.align_tags_names_descriptions` | `JD_ALIGN_PARAM_COMMENTS` | false |
| `comments.indent-tag-description` | `comment.indent_tag_description` | `JD_INDENT_ON_CONTINUATION` | true |
| `comments.leading-asterisks` | — | `JD_LEADING_ASTERISKS_ARE_ENABLED` | true |
| `comments.normalize-parameter-comments` | — | — | `CommentsHelper.reformatParameterComment`（固定） |
| `comments.inline-block-comments` | — | — | 固定 |
| `imports.order` (`preserve`/`sort`/`group`) | —（JDT formatter は import を触らない） | `IMPORT_LAYOUT_TABLE` の有無 | `ImportOrderer`（常に group） |
| `imports.groups` | — | `IMPORT_LAYOUT_TABLE` | `["static", "*"]` 固定 |
| `imports.static-first` | — | `LAYOUT_STATIC_IMPORTS_SEPARATELY` | true |
| `imports.reorder-modifiers` | — | — | `ModifierOrderer`（固定 true） |
| `literals.*` | — | — | すべて `preserve`（GJF はリテラルを書き換えない） |

---

## 6. 「1 つも欠落しない」をどう検証するか

宣言ではなく**テストで守る**。各 importer の `tests` が持つのは次の 3 本。

1. **カバレッジ（`inventory.tsv` 駆動）** — `every_inventoried_option_is_modeled`。目録の各行について
   「その id だけを含む設定マップ」を作り、モデルへ deserialize した結果が `Model::default()` と
   **異なる**ことを表明する。1 つでもモデルに無い id があればテストが落ち、落ちた id が列挙される。
   目録は §0 の一次情報から機械抽出したものなので、「テストが通る ⇔ ベンダー option を 1 つも
   落としていない」が成立する。目録自体が縮まないよう、行数も
   `the_inventory_is_the_documented_size` で固定している（Eclipse 416 / IntelliJ 297）。
2. **2 つの綴りの一致** — IntelliJ は同じ設定を XML 名と `.editorconfig` キーの 2 通りで書く。
   `every_editorconfig_key_resolves_to_its_setting` が目録と生成キー表の同期を、
   `editorconfig_and_xml_spellings_agree` が「同じ設定を両形式で書いたら同じモデルになる」ことを
   確かめる。Eclipse も `the_xml_profile_and_the_prefs_file_agree` で同じ性質を持つ。
   これが無いと、片方の形式でだけ設定が無言で落ちる。
3. **到達性** — `every_config_section_is_reachable_from_some_vendor`。`jals_config::fmt::Config` の
   各節が、少なくとも 1 つのベンダー写像で既定から動くことを表明する。`[imports]` は Eclipse からは
   決して動かない（JDT formatter は import を触らない）ので GJF が、`[literals]` はどの importer からも
   動かない（4 者一致）ので **jals-native であることを明示的に記録**している。

§2 の基準の「投機的な rule を置かない」は 3 が守り、§3 の「native モデルは全数」は 1 と 2 が守る。

## 7. 意図的に**写像しない**もの（欠落ではない）

native モデルには載せるが、`Config` へは写さない。理由を型で残すため、モデル側では
専用の構造体（`IntellijNaming` / `IntellijCodegen` など）に隔離する。

| 群 | 例 | 写さない理由 |
|---|---|---|
| IntelliJ 命名規約 | `FIELD_NAME_PREFIX`, `TEST_NAME_SUFFIX`, `PREFER_LONGER_NAMES`, `VISIBILITY` | コード生成・inspection の設定であってフォーマッタ rule ではない |
| IntelliJ コード生成 | `INSERT_OVERRIDE_ANNOTATION`, `GENERATE_FINAL_LOCALS`, `REPLACE_INSTANCEOF`, `REPLACE_SUM` | 同上 |
| IntelliJ 意味論依存 | `CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND`, `NAMES_COUNT_TO_USE_IMPORT_ON_DEMAND`, `PACKAGES_TO_USE_IMPORT_ON_DEMAND`, `DELETE_UNUSED_MODULE_IMPORTS` | wildcard 集約・未使用判定は classpath / 名前解決を要する（`DESIGN.md` §11 結論 4, P-gen-3） |
| IntelliJ `IMPORT_LAYOUT_TABLE` の module 行 | `<package name="" module="true"/>` | `import module M;` を**名前 prefix ではなくプロジェクト構造**で選ぶ行。jals の `imports.groups` は生の文字列 prefix マッチなので写像先が無い。`PackageEntry::is_module` として型付きで保持し、射影では**行ごと落とす**（name が空なので、落とさないと catch-all 群が二重に出る） |
| IntelliJ `IMPORT_LAYOUT_TABLE` の `withSubpackages` | `java.*`（当該パッケージのみ） vs `java.**`（配下含む） | jals の `imports.groups` は生の文字列 prefix マッチで、**非再帰の prefix を表す形が無い**。両者とも `"java."` に潰れる（＝`java.*` 指定でも `java.util.*` を巻き込む）。§2 の基準「2 つ以上の到達可能なターゲットが食い違う」を満たさない — この概念を持つのは IntelliJ だけで、Spotless `importOrder` にも GJF にも非再帰の形は無く、Eclipse は import を触らない。よって投機的な rule を足さず、`PackageEntry::with_subpackages` として型付きで保持するに留める |
| IntelliJ エディタ挙動 | `WRAP_ON_TYPING`, `FORCE_REARRANGE_MODE`, `KEEP_BUILDER_METHODS_INDENTS` | 入力中の挙動・rearrange ダイアログ設定でバッチ整形の出力に効かない |
| IntelliJ 整列 | `ALIGN_MULTILINE_*` 18 個, `ALIGN_CONSECUTIVE_*` | **列揃え**は幅計算が入力に依存し、jals の canonical レイアウトモデルに乗らない（`DESIGN.md` §13 の L2 = engine 固有）。native モデルには全数保持し、互換 engine 移植時に使う |
| Eclipse 整列 | `align_type_members_on_columns`, `align_variable_declarations_on_columns`, `align_assignment_statements_on_columns`, `alignment_for_*` の `M_INDENT_ON_COLUMN` ビット | 同上 |
| Eclipse コメント微細 | `comment.javadoc_paragraphs_tags_with_content`, `comment.new_lines_at_javadoc_boundaries` ほか | Javadoc 整形器を移植するまで写像先が無い。native 側に保持 |

**この表に載っていることが「構造化された」の意味**である。写像表（§5）に現れない native option も、
モデル上は型を持ち、名前を持ち、テスト 1 が存在を保証している。

---

## 8. 残る限界（`DESIGN.md` の再掲・更新）

- **P-gen-1（統一不能）は解消していない。** 本書の写像は「共通語彙への射影」であって全単射ではない。
  bit 一致には `DESIGN.md` §14 の pluggable engine が要る。
- **P-gen-2（空白依存）**: `KeepOnOneLine::Preserve` / `join-wrapped-lines` / `blank-lines.max-*` は
  入力空白の関数であり、§17 の whitespace-retaining モードでしか意味を持たない。canonical モード
  （GJF/Palantir/jals-native）ではそれぞれ決定的な既定値に丸める。
- **P-gen-4（Spotless DSL）**: `build.gradle` / `pom.xml` はコードなので、`SpotlessConfig` は
  **解決済みパイプライン**をモデル化する。DSL テキストからの値抽出は本 importer の対象外。
- **P-gen-6（非既定のみ透過）**: 目録に既定値を持たせているのは、生成 toml に非既定 option だけを
  書き出せるようにするため。
