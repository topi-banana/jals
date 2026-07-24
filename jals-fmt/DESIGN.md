# jals-fmt 設計: 複数フォーマッタ完全互換

> 目的: `jals-fmt` を、既存 Java フォーマッタ (**google-java-format / Spotless / Eclipse JDT /
> IntelliJ IDEA**) と **byte 単位で一致**させられる基盤にする。さらに、これらの native config が
> 存在し `jalsfmt.toml` が無い場合に**互換 toml を自動生成**する。本書は必要な rule/behavior の
> リストと、それらが**この構造化 (5 層パイプライン + Doc-IR) に載るか**の判定、および Rust 表現を定める。
>
> **本書の構成:**
> - **Part I (§0–§10)**: google-java-format (GJF) を単一ターゲットとして深掘り。IR/エンジン/rule/
>   不変条件の基準系を定義する。
> - **Part II (§11–§17)**: 対象を Spotless / Eclipse / IntelliJ へ拡張。**4 者が相互非互換な 4 つの
>   エンジン**である事実と、それが本構造化に与える帰結（pluggable engine、入力空白依存、config 自動
>   生成の限界）を洗う。
>
> 全フォーマッタの内部仕様は一次情報 (各 OSS の `master` ソース + 公式仕様) から確認した。GJF の
> ソース参照は `core/src/main/java/com/google/googlejavaformat/` 配下のパスで示す。

---

# Part I — google-java-format 100% 互換

## 0. スコープと非スコープ

対象: GJF CLI の既定 `format` が行う整形の完全再現（GOOGLE スタイル / AOSP バリアント）。

**先に確定すべき事実 — GJF は設定不可。** README 明言:「フォーマットアルゴリズムに設定可能性
は無い。単一フォーマットへ統一するための意図的な設計判断である」。バリアントは AOSP のみで、
差は**indent 倍率だけ**（`JavaFormatterOptions`: `GOOGLE(1)` / `AOSP(2)` → block 2/4, 継続 4/8,
列上限は共に 100）。

**帰結（本設計の背骨）:** GJF 互換は「jals の設定オプションを適切な既定値に並べる」ことでは
**達成できない**。GJF は独自の**レイアウトエンジン**と**構文別の Op 発行**を持ち、その両方を
移植して初めて一致する。よって `jals-fmt` は 2 つの整形パス（jals 独自 style / GJF 互換）を持つ
のではなく、**GJF のエンジンと IR を中核に据え、jals 独自スタイルはその上の Op 発行の差分として
表現する**（§8）。GJF の `Break` 語彙 (`UNIFIED`/`INDEPENDENT`/`FORCED` + `BreakTag`) は
prettier の group/fill の上位互換であり、jals 独自オプションもこの語彙で表せるためこの統合が成立する。

---

## 1. 結論を先に — 「すべての rule が並列に表現できるか」

**否。だが「否」の中身を正確に切り分けることが答えの核。**

まず一番鋭い言い方: **「何を発行するか」(emission) は並列・合成的だが、「折りをどう解決するか」
(resolution) は本質的に逐次であり、並列化できない。**
- **発行 (declarative)**: 各構文は独立に自分の Doc 部分木を寄与する。これは L2 であり、真に並列
  （構文ごとに別々の visitor が合成される）。
- **解決 (evaluative)**: `computeBreaks` は左→右の畳み込みで、`mustBreak` を前方伝播し `BreakTag` を
  相互参照する**単一パスの状態機械**。GJSG §4.5.1「まず最高位の構文レベルで折る」は、折り判定が
  ネストを跨いで**エンジンの共有状態で結合している**ことを意味する。ゆえに resolution は原理的に逐次。

この emission/resolution の分離が、下の L1/L2 の切り分けの実体である。

**GJF 完全互換の rule 集合は、互いに並列ではない 5 層のパイプラインに階層化される。**
前段で提案した平坦な 3 族分類 (text-normalization / sequence-reordering / layout) は
**jals 独自の設定オプション**向けの分類であり、GJF 互換ではそのまま使えない。GJF 向けには次の
**層 (Layer)** に再キャストする必要がある。層内は並列だが、**層と層は並列ではなくパイプライン**。

| 層 | 内容 | 層内の並列性 | 前段の構造化に載るか |
|---|---|---|---|
| **L0 前処理トークンパス** | import 整列 / 未使用 import 削除 / modifier 整列 | ○ 並列 | sequence-reordering 族 + 全木解析 1 個 |
| **L1 レイアウトエンジン** | `computeBreaks`（GJF 固有アルゴリズム）+ IR 語彙 | — (単一の基盤) | **rule ではない**。汎用 prettier printer を**置換**する |
| **L2 構文別 Op 発行** | 約 50 の visitor（構文ごとの折り返し） | ○ 並列 | layout 族（ただし GJF の忠実移植であり自由設計ではない） |
| **L3 コメント / Javadoc** | コメント付着 + Javadoc 再整形 | △ 付着は枠組み、Javadoc は独立 | 付着=枠組み。**JavadocFormatter は入れ子の別フォーマッタ**で並列でない |
| **L4 後処理パス** | StringWrapper + 最終化 | — | **StringWrapper は出力を再パースする第 2 パス**で並列でない |

- **並列に載る大多数 = L2**（rule 数で言えば大半）。ここは CST→Doc lowering として一様・並列に
  表現でき、提案構造化と綺麗に噛み合う。
- **並列に載らない 4 点**が 100% 互換の要:
  1. **L1 エンジン**は自由に選べない。GJF の `computeBreaks` を移植する必要があり、旧 jals の
     prettier 風 `render`/`fits` では bit 一致しない（§2.2, §6.2）。
  2. **JavadocFormatter** (L3) は Javadoc 文法の入れ子ミニフォーマッタで、構文 rule とは並列でない。
  3. **未使用 import 削除** (L0) は全木の名前収集を要する大域解析で、per-node rule ではない
     （ただし**型解決は不要**＝CST だけで完結する。§4.L0）。
  4. **StringWrapper** (L4) は整形済み出力を**再パースして再レイアウトする第 2 パス**。
     不動点検証まで含み、通常の Doc lowering には還元できない。

したがって答えは:「**L2 は提案構造化に完全に並列で載る。だが 100% 互換には、rule ではない
固定エンジン (L1) と、並列でない 3 つのサブシステム (L3 Javadoc, L0 未使用 import, L4 StringWrapper)
を別ステージとして加える必要がある**」。

---

## 2. GJF の実像（一次情報）

### 2.1 入力木 — javac AST（jals は rowan CST）
`JavaInputAstVisitor` (4182 行) は `com.sun.source.util.TreePathScanner` を継承し、**javac の
parse tree** (`com.sun.source.tree.*`) を走査して Op 列を発行する。jals は rowan CST から
lowering する。**両者は木の粒度が違う**（例: javac の `MethodInvocation`+`MemberSelect` 連鎖 vs
jals の `CALL_EXPR`/`FIELD_ACCESS`）。GJF の Op 発行を**jals の CST 形状の上で等価に再導出**する
のが L2 の本質であり、最大の工数。トークン列は等価なので原理的には可能。

### 2.2 レイアウトエンジン — `Doc.Level.computeBreaks`（最重要）
GJF のレイアウトは **greedy・単一パス・再帰下降でバックトラックしない**（`Doc.java` verbatim 確認）。

```java
State computeBreaks(commentsHelper, maxWidth, state) {
  int thisWidth = getWidth();                       // ボトムアップで前計算した「この Level の平坦幅」
  if (state.column + thisWidth <= maxWidth) {        // ← Level 自身の幅で判定。境界で止まる
    oneLine = true; return state.withColumn(state.column + thisWidth);
  }
  State broken = computeBroken(commentsHelper, maxWidth,
      new State(state.indent + plusIndent.eval(), state.column)); // 折れる時 indent を加算
  return state.withColumn(broken.column);
}
```

per-break 判定 (`computeBreakAndSplit`):

```java
boolean shouldBreak =
    (break.fillMode == UNIFIED)                       // UNIFIED は全 or 無で一斉に折れる
    || state.mustBreak                                 // 直前 split が溢れたら次は強制
    || state.column + breakWidth + splitWidth > maxWidth; // INDEPENDENT は次要素が入らない時だけ折る (=fill)
```

**prettier との決定的な差（＝汎用 printer では一致しない理由）:**
- GJF の平坦判定は **Level 自身の前計算幅**を見て**Level 境界で止まる**。prettier の `fits` は
  group を**越えて**次の hard newline まで前方走査する。⇒ 同じ行に後続がある時の折り判定が食い違う。
- GJF は 1 つの Level 内に `UNIFIED`（全 or 無）と `INDEPENDENT`（fill）を**混在**させる。
  prettier は group（全 or 無）と fill を別ノードに分ける。
- `state.mustBreak` の**前方伝播**（ある split が溢れたら次の break を強制）は prettier に対応物なし。
- 幅は `getWidth()` のボトムアップ前計算で、`FORCED` break や改行を含む Token は幅を巨大 sentinel
  (`MAX_LINE_WIDTH`) に汚染し、その Level は**決して平坦にならない**
  (`Break.computeWidth: isForced() ? MAX_LINE_WIDTH : flat.length()`)。

**結論: 100% 一致には `computeBreaks` / `computeBroken` / `computeBreakAndSplit` / `getWidth` を移植する。**

### 2.3 IR 語彙 — Doc / Break / Level（prettier 語彙ではない）
`Doc` サブクラスは 5 つ: `Level`, `Token`, `Break`, `Space`, `Tok`（**コメントと空白は `Tok`
として Doc 木の中に載る**。独立の `Comment` クラスは無い）。

- `Level { Indent plusIndent; List<Doc> docs; ... }` — 自前 indent を持つグループ。
- `Break { FillMode fillMode; String flat; Indent plusIndent; Optional<BreakTag> optTag; }`
  - `flat`: 折れない時に出すテキスト、`plusIndent`: 折れた時の追加 indent、
  - `optTag` (`BreakTag`): **相関ブレーク** = prettier の group-id / `if_break(id)` に相当。
    前段で「追加すべき」と挙げた group-id 機構は GJF に既にある。
- `FillMode`: `UNIFIED`（Level が入らなければ全 UNIFIED break が折れる）/ `INDEPENDENT`（fill）/
  `FORCED`（常に折れ、その Level は平坦不可）。
- `Indent` は生 int ではなく型: `Const`（`make(n, multiplier)` で倍率を焼き込む）と、**`BreakTag`
  に条件づく合成 indent**（`Indent.If`）がある。

### 2.4 定数
`Formatter.MAX_LINE_LENGTH = 100`（両スタイル共通）。`Indent.Const.make(±2, mult)` /
`make(±4, mult)`、`mult = GOOGLE(1)|AOSP(2)`。⇒ **GOOGLE: block +2 / 継続 +4、AOSP: +4 / +8**。

### 2.5 パスの順序（CLI 既定）
`formatSourceAndFixImports`:

```
ImportOrderer.reorderImports → RemoveUnusedImports → formatSource(core) → StringWrapper.wrap
```

`ModifierOrderer`（トークン列上の純構文操作、既定 on）、`JavadocFormatter`（コメント描画経路で
`formatJavadoc` gating）も既定 on。既定 on の behaviour は各 `--skip-*` で無効化: import 整列 /
未使用 import 削除 / 長文字列再折り / Javadoc 整形。**トークンを変える操作は 4 つだけ**:
(1) import 整列, (2) 未使用 import 削除, (3) modifier 整列, (4) 長い文字列連結の再折り返し。
それ以外は純粋に空白・改行・コメントのレイアウトで、**リテラルを含む有意トークンは不変**。

### 2.6 幅計測 — `String.length()`（UTF-16 単位、display-width ではない）
`Break.computeWidth`/`Token` 幅は `flat.length()`（UTF-16 code unit 数）。GJF は **East-Asian-width を
使わない**。CJK 等の全角も幅 1 と数える。旧 jals は `UnicodeWidthStr::width`（表示幅）を使っており、
**wide 文字で GJF と乖離する**。§7 の bit-exact 要件。

### 2.7 コメント付着（GJF の規則）
2 段階 (`JavaInput.buildToks` + `OpsBuilder.build`):
1. **行ベース付着**: 直前トークンと**同一行**のコメント/空白は `toksAfter`（trailing）。改行を跨ぐ
   と以降は次トークンの `toksBefore`（leading）。例外: `/* */` は `(` `<` `.` の直後には付着しない、
   Javadoc は `;` の直後に付着しない、`/*name=*/` 形のパラメータコメントは常に新トークンを開始。
2. **Op への挿入時の FillMode**: 行コメント `//` は **`FORCED`**（強制改行）、ブロック `/* */` は
   **`UNIFIED`**。trailing block comment は `breakAndIndentTrailingComment` で自前行へ落とせる。

---

## 3. アーキテクチャ: 5 層パイプライン

```
                              ┌─────────────────────────────── L4 後処理
 source(&str)                 │  StringWrapper(再パース→長文字列連結を再折り→不動点検証)
   │                          │  finalize(trailing 空白除去 / 末尾改行 / 連続空行を1へ / LF)
   ▼                          ▼
 ┌───────── L0 前処理トークンパス ─────────┐    ┌─── 出力 String
 │ ImportOrderer(整列)                      │    │
 │ RemoveUnusedImports(全木名前収集)         │    ▲
 │ ModifierOrderer(正準順)                   │    │
 └──────────────┬───────────────────────────┘    │  L1 エンジン: doc.computeBreaks(100)
                ▼                                  │      + write(Tok/コメント描画, L3)
 parse → rowan CST ── L2 visitor ──► Doc 木 ───────┘
                     (構文別 Op 発行)        ▲
                                             │ L3: コメント付着(枠組み) / JavadocFormatter(入れ子)
```

モジュール構成案（前段の core/rules 分離を踏襲）:

- `ir` — GJF 忠実な `Doc`/`Break`/`Level`/`FillMode`/`BreakTag`/`Indent`（§6.1）。
- `ops` — `OpsBuilder` 相当（`open/close/space/breakOp/breakToFill/forcedBreak/token/blankLineWanted`）。
- `engine` — `computeBreaks` 移植（§6.2）と `write`。
- `visit` — L2 構文別 lowering（rowan CST → Ops）。
- `comments` — L3 付着枠組み（GJF の行ベース規則）。
- `javadoc` — L3 JavadocFormatter（入れ子ミニフォーマッタ）。
- `passes` — L0 (`import_order`, `unused_imports`, `modifier_order`) と L4 (`string_wrapper`,
  `finalize`)。
- `style` — GOOGLE/AOSP の倍率など固定パラメータの Reify。

---

## 4. 必要な rule / behavior の完全リスト

各項目に **[族/層]**、**GJF 出典**、**構造化への適合**を付す。適合列:
`◎`=L2 の per-node lowering に並列で載る / `○`=sequence-reordering 族 / `△`=枠組みだが独立機構 /
`✗`=並列でない別ステージ。

### L0 前処理トークンパス

| # | rule | 出典 | 適合 | 備考 |
|---|---|---|---|---|
| R0.1 | **import 整列**: static→module→normal(ordinal)、各群内 ASCII 昇順、群間に空行 1、重複除去、決して折り返さない | `ImportOrderer.java`, GJSG §3.3.3 | ○ | **static が先**（IntelliJ 既定と逆）。plan/emit 分離、元ノード再利用で多重集合保存 |
| R0.2 | **未使用 import 削除**: 全 `IDENT` 単純名 + Javadoc `@link/@see/@throws` 参照名を収集し、単純名が未出現の import を削除 | `RemoveUnusedImports.java`, README | ✗(大域) | **型解決不要**の名前ヒューリスティック＝CST のみで再現可。per-node でない全木パス。shadowing は GJF も取りこぼす（それも再現） |
| R0.3 | **modifier 整列**: `public protected private abstract default static sealed non-sealed final transient volatile synchronized native strictfp`（JLS/`javax...Modifier` ordinal） | `ModifierOrderer.java`, GJSG §4.8.6 | ○ | 純構文。annotation は先頭へ保持、各コメントは modifier に随伴 |

### L1 レイアウトエンジン（rule ではない・固定基盤）

| # | 項目 | 出典 | 適合 |
|---|---|---|---|
| E1 | `computeBreaks`/`computeBroken`/`computeBreakAndSplit`/`getWidth` の意味論（§2.2） | `Doc.java` | 汎用 printer を置換 |
| E2 | 列上限 100、block indent 2×mult、継続 4×mult | `Formatter/Indent/JavaFormatterOptions` | 固定 |
| E3 | 幅 = UTF-16 code unit（§2.6） | `Doc.computeWidth` | display-width 不可 |
| E4 | `BreakTag` 相関ブレーク、`Indent.If(tag)` 条件 indent | `Doc/Indent.java` | IR 必須語彙 |

### L2 構文別 Op 発行（すべて ◎ = 並列に載る）

まとめて記す。各 rule は「対応 CST ノード種 → GJF が発行する open/break/token/close の等価再現」。

- **コンパイル単位**: `package`（折り返さない）/ import ブロック / 型宣言列 / メンバ間の空行方針。
- **モジュール宣言** (`module-info.java`): `module`/`open module` 本体と、`requires`（`transitive`/
  `static` 修飾つき）/ `exports`/`opens`（`to` 節）/ `uses`/`provides ... with` の各ディレクティブ。
  `requires transitive static` の修飾子順も modifier 整列 (R0.3) の対象。
- **型宣言**: class / interface / enum / record / annotation-type。修飾子、型パラメータ `<>`（内側
  空白なし）、`extends`/`implements`/`permits`（高位で折り、`&` は空白あり）、本体ブレース K&R。
  **trivial enum の 1 行化**（メンバ宣言も定数 doc も無い enum は配列初期化子扱いで収まれば 1 行）。
- **メンバ**: field（複数 field annotation は同一行可）/ method / constructor / 初期化子ブロック /
  パラメータ列 / `throws` / receiver 파라미터。
- **文**: block / local-var（分割しない）/ expression-stmt / if-else / for / for-each（`:` は
  代入系＝後折り）/ while / do-while / switch 文(colon 形、fall-through コメントは合成しない) /
  synchronized / try-catch-finally / try-with-resources / return / throw / yield / assert /
  labeled / break / continue / empty。
- **式**: binary（**演算子の前で折る**、両側空白）/ unary / ternary（`?`/`:` の**前で折る**）/
  assignment（`=` の**後で折る**、両側空白）/ cast（`)` の後に空白）/ instanceof(+pattern) /
  method 呼び出し・**メソッドチェーン（`.` の前で折る）** / field access / array access /
  new（class/anon/array）/ 配列初期化子（block-like、pack 方式は未仕様＝GJF 挙動を実測して合わせる）/
  lambda（`->`、単一無中括弧式のみ矢印直後で折り可）/ method reference（`::` の前で折る・両側空白なし）/
  switch 式(arrow 形) / parenthesized / **リテラル（逐語・不変）**。
- **型**: primitive / class・generic（`<>` 空白なし）/ array（`[]` は前トークンに密着、型注釈と `[]`/
  `...` の間は空白）/ varargs `...` / wildcard / intersection（`&` 空白あり）/ multicatch union
  （`|` 空白あり）/ `var` / 型使用注釈（型の直前・インライン）。
- **パターン**: type pattern / record deconstruction / guarded (`when`)。
- **注釈配置**: 宣言注釈（class/method/package/module）は各 1 行、field 注釈は複数同一行可、
  単一無引数注釈は method シグネチャ 1 行目に載せてよい、パラメータ/型使用注釈はインライン。
  **[SEMANTIC 注意]** GJSG の「型使用 vs 宣言」判定は `@Target` を要するが、GJF は**構文位置**で
  置くだけ（型解決しない）。jals も同じく構文位置で置く。
- **text block**: 開き `"""` は改行後、閉じ `"""` は開きと同 indent の自前行、内部は文字列データ
  として保持（内容は reflow しない）。
- **空ブロック**: `{}` 簡潔形（ただし if/else・try/catch など多ブロック文では簡潔形を使わない）。
- **水平空白**: GJSG §4.6.2 の「空白が現れるのはこの列挙のみ」を厳密実装（キーワードと `(`、
  `}` と `else/catch`、`{` の前、二項/三項演算子の両側と `& | : -> `、`, : ;` と cast の `)` の後、
  型と識別子の間、型注釈と `[]`/`...` の間、`//` 周り。**`::` と `.` の周りは空白なし**、`<>` 周り
  も空白なし）。
- **空行**: メンバ間はちょうど 1、連続空行は 1 へ畳む、field 間は任意保持。

> 補足: GJF が**やらない**構造変更は jals もやらない（braces 挿入、`int a,b;` 分割、overload 並べ替え、
> `default`/網羅性追加、wildcard 展開、リテラル書換え、long suffix `L` の大文字化）。これらは
> GJSG にはあるが GJF 非実装であり、100% 互換のため**実装してはならない**。

### L3 コメント / Javadoc

| # | rule | 出典 | 適合 | 備考 |
|---|---|---|---|---|
| R3.1 | **コメント付着**: 行ベース（同一行 trailing / 改行後 leading）、`/* */` は `( < .` 直後に付着せず、Javadoc は `;` 直後に付着せず、`/*name=*/` は新トークン開始。`//`=FORCED、`/* */`=UNIFIED | `JavaInput/OpsBuilder.java`, §2.7 | △枠組み | 旧 jals の CommentMap を**GJF の付着規則に差し替え**て再利用可 |
| R3.2 | **JavadocFormatter**: Javadoc を字句解析し、HTML ブロック/インラインタグ（`<p><br><pre><ul><li>…`）・ブロックタグ（`@param/@return/@throws`）・`{@code}`/fenced・`///` Markdown を扱い、prose を 100 桁へ reflow、収まれば 1 行化 | `javadoc/JavadocFormatter.java` | ✗独立 | **Javadoc 文法の入れ子ミニフォーマッタ**。構文 rule と並列でない。gating: `formatJavadoc` |

### L4 後処理

| # | rule | 出典 | 適合 | 備考 |
|---|---|---|---|---|
| R4.1 | **StringWrapper**: 整形済み出力を再パースし、100 桁超の文字列/文字列連結を検出、`+` 連結を平坦化して語/エスケープ境界で分割・継続 indent 付きで再発行、text block を再 indent、pretty-print が不変であることを検証してから採用 | `StringWrapper.java` | ✗第2パス | 出力テキストに対する**第 2 のフォーマッタ**。不動点検証つき。単一リテラルを新トークンへ割ることは非確認（やらない） |
| R4.2 | **finalize**: trailing 空白除去 / 末尾改行 1 / 連続空行 1 / LF 正規化 | observed | ◎ | エンジンの出力段で決定的に |

### GJF 互換で **無効化**すべき jals 独自オプション

GJF はリテラルを一切書き換えないため、以下は**必ず off/固定**:
`hex-literal-case`, `float-literal-trailing-zero`, `literal-suffix-case`（すべて Preserve）、
`normalize-parameter-comments`（GJF は `/*name=*/` を特別扱いするが大文字小文字正規化はしない —
実測要）、`space-*`/`brace-style`/`binop-*`/`fn-params-layout` 等の layout 系はすべて GJF 固定値に
束縛。text-normalization 族は **GJF 互換では空**。

---

## 5. 並列表現可能性の判定（詳細）

前段の 3 族分類を GJF 互換に**写像**すると:

- **layout 族 → L2**（◎, ~50 rule, 並列）: 提案どおり per-node lowering に一様に載る。**最大の塊が
  綺麗に並列化できる**。ただし各 lowering の中身は「自由なスタイル設計」ではなく「GJF Op 発行の
  忠実移植」に固定される。
- **sequence-reordering 族 → L0 の import/modifier**（○, 並列）: plan/emit 分離＋元ノード再利用の
  規律がそのまま活きる。多重集合保存。
- **text-normalization 族 → 空**: GJF はリテラルを変えない。この族は GJF 互換では使わない。

**3 族に載らない=並列でない 4 要素**（§1 再掲、これが 100% 互換の本質的な追加コスト）:

1. **L1 エンジン**: rule ではなく基盤。汎用 prettier printer を GJF `computeBreaks` で**置換**する。
   これは「並列に足す rule」ではなく「土台の入れ替え」。
2. **未使用 import 削除 (L0/R0.2)**: per-node でなく**全木 1 パスの名前解析**。型解決は不要なので
   `jals-hir` は要らず jals-fmt 内で完結するが、族としては新カテゴリ「全木構文解析パス」。
3. **JavadocFormatter (L3/R3.2)**: Javadoc 文法の**入れ子フォーマッタ**。構文 rule と直交する別機構。
4. **StringWrapper (L4/R4.1)**: 整形出力を再パースする**第 2 パス**＋不動点検証。パイプライン後段。

**総括**: 「すべての rule が並列に表現できるか」への答えは **「L2 は完全に並列。ただし 100% 互換は
L2 だけでは閉じず、rule でない固定エンジン 1 と、並列でないサブシステム 3 を段として加える必要が
ある。3 族分類は GJF 向けには 5 層パイプラインへ再構成する」**。

---

## 6. GJF 忠実な Rust 表現

### 6.1 IR（`Doc`/`Break`/`Level`/`Indent`）— prettier 版ではなく GJF 版

```rust
/// GJF `Doc` の 5 サブクラスに対応（コメント/空白は Tok として木に載る）。
enum Doc {
    Level(Level),
    Token { text: Box<str>, plus_indent_comments_before: Indent,
            break_and_indent_trailing_comment: Option<Indent> },
    Break(Break),
    Space,                      // 非改行スペース（幅 1）
    Tok(Box<str>),              // コメント/空白の逐語テキスト（改行含みうる）
}

struct Level {
    plus_indent: Indent,        // この Level が折れた時に足す indent
    docs: Vec<Doc>,
    // 描画時に確定するキャッシュ（getWidth のボトムアップ結果など）は別持ち
}

#[derive(Clone, Copy)]
enum FillMode { Unified, Independent, Forced }   // 全or無 / fill / 強制

struct Break {
    fill_mode: FillMode,
    flat: Box<str>,             // 折れない時に出すテキスト（多くは "" か " "）
    plus_indent: Indent,        // この break が折れた時の追加 indent
    tag: Option<BreakTag>,      // 相関ブレーク（prettier group-id 相当）
}

#[derive(Clone, Copy, PartialEq, Eq)] struct BreakTag(u32);

/// GJF の Indent。生 int ではなく型（BreakTag 条件つき indent を表現するため）。
enum Indent {
    Const(i32),                 // n * indentMultiplier を構築時に焼き込む
    If { tag: BreakTag, broken: Box<Indent>, flat: Box<Indent> },
    // 必要なら Add(Vec<Indent>) など GJF の合成に対応
}
```

`OpsBuilder` 相当の発行 API（visitor はこれ経由で Ops を積む）:

```rust
impl Ops {
    fn open(&mut self, plus_indent: Indent);
    fn close(&mut self);
    fn space(&mut self);
    fn break_op(&mut self, fill: FillMode, flat: &str, plus_indent: Indent, tag: Option<BreakTag>);
    fn break_to_fill(&mut self);       // FillMode::Independent の糖衣
    fn forced_break(&mut self);        // FillMode::Forced
    fn token(&mut self, text: &str, plus_indent_comments_before: Indent,
             break_and_indent_trailing: Option<Indent>);
    fn blank_line_wanted(&mut self, w: BlankLineWanted);
}
```

**幅は UTF-16 単位**（§2.6）:

```rust
/// GJF は String.length()（UTF-16 code unit 数）で列を数える。display-width にしない。
fn gjf_width(s: &str) -> usize { s.chars().map(|c| c.len_utf16()).sum() }
```

### 6.2 エンジン（`computeBreaks` 移植）

```rust
/// GJF Doc.Level.computeBreaks の忠実移植。greedy・単一パス・バックトラックなし。
/// prettier の fits（group を越えて前方走査）と混同しないこと。
fn compute_breaks(level: &mut Level, max: usize, st: State) -> State {
    let w = get_width(level);                 // ボトムアップ前計算。Forced/改行は巨大 sentinel
    if st.column + w <= max {                 // ← Level 自身の幅で判定、境界で止まる
        level.one_line = true;
        return st.with_column(st.column + w);
    }
    let broken = compute_broken(level, max,
        State { indent: st.indent + eval(&level.plus_indent, &st.tags), column: st.column, must_break: false });
    st.with_column(broken.column)
}
// compute_broken: splitByBreaks → split[0] を処理し、以降 (break, split) 対を computeBreakAndSplit で処理。
//   shouldBreak = fillMode==Unified || st.must_break || st.column + break_w + split_w > max;
//   split が溢れたら次 break の must_break=true（前方伝播）。
//   ネストした Level は computeSplit 内で再帰的に独自の平坦判定を行う（再フロー・戻りなし）。
```

`BreakTag` は 2 パス: 描画中に各 tag の break/flat が確定し、`Indent::If` はその確定値を参照する
（GJF と同じ相関）。

### 6.3 L2 visitor（構造化の並列部分）

前段で示した「値を返す構築コンビネータ + `Format`/`write!` の是非」は、GJF では **OpsBuilder への
命令的発行**が原典なので、buffer-write 型（`fn visit(&mut Ops, node)`）が自然に一致する。
async/yield は前段 §3c の結論どおり **build は sync + driver で粗く yield、engine 描画は
async(Yielder)** とすれば、per-node ボクシングなしで OpsBuilder 型がそのまま使える
（`!Send` は sync な visitor に無関係）。深い入れ子の stack 安全は §7。

```rust
trait Visit { fn visit(&self, node: &SyntaxNode, ops: &mut Ops) -> FormatResult<()>; }
// ノード種ごとに 1 実装。未対応/ERROR ノードは verbatim へ fallback（§9）。
```

### 6.4 L0/L4 パス

- L0: `plan_imports(cst) -> ImportPlan`（純関数、元ノード再利用で emit）、`collect_used_names(cst)
  -> NameSet`（全 IDENT + Javadoc 参照）、`plan_modifiers(modifiers) -> Order`。
- L4: `wrap_long_strings(formatted: &str) -> String`（再パース→再折り→不動点検証）、
  `finalize(buf) -> String`。

---

## 7. bit-exact 達成の落とし穴（実測で潰す）

1. **幅計測**: UTF-16 単位（§2.6）。旧 `UnicodeWidthStr` は wide 文字で乖離。
2. **エンジン差**: prettier 風 `fits` を絶対に流用しない（§2.2）。境界前方走査と mixed-fill が違う。
3. **CST↔javac AST 写像**: メソッドチェーン・二項式のネスト・注釈の型使用/宣言判定など、木形状の
   違いで Op 構造がずれると折り位置が変わる。**GJF 実測出力を golden にした差分テスト**が必須。
4. **配列初期化子の pack 方式**・**arg/param の折り単位**は GJSG 非仕様＝GJF 実挙動を golden で固定。
5. **StringWrapper の不動点**: GJF は wrap 後に AST 不変を検証。jals も同検証を入れないと発散しうる。
6. **AOSP 倍率**: indent/継続のみ差、列上限は 100 のまま。
7. **改行/末尾**: LF・末尾改行 1・連続空行 1（GJSG 非明記の observed 挙動）。

### 7.1 「100%」の定義 — version pin と golden harness（必須の基盤）

**「100% 互換」は GJF のバージョンを固定して初めて定義できる。** GJF の出力は**リリース間で意図的に
非安定**（だから Spotless 等は GJF バージョンを pin する）。したがって:

1. **互換ターゲットを特定バージョンに固定**する。「100% 互換」= 「GJF `vX.Y.Z` と byte 一致」であり、
   それより緩い定義は無い。バージョンを上げるのは明示的な意思決定とする。
2. **golden corpus が目標の唯一の操作的定義**。本書の各「GJF 実測要」項目（配列 pack、arg/param 折り
   単位、幅計測、import 順、text block 内部 indent、StringWrapper 挙動…）は、すべてこの 1 つの
   harness で定義かつ検証される: 実 GJF を Java コーパス（JDK / 主要 OSS）に通した出力を golden とし、
   `jals-fmt` 出力と diff する。不一致は最小再現へ縮約して visitor/engine を修正。
3. **これは純 Rust ワークスペースに JVM + GJF を CI 依存として持ち込む**インフラ判断（現 `CLAUDE.md`
   に JVM 依存は無い）。実行方法は要検討: (a) CI で GJF を実行し golden を生成、(b) golden を生成物
   としてリポジトリに commit（JVM を通常ビルドから隔離、`xtask`/fixture 生成のみ JVM 使用）、
   のいずれか。`jals-tests` の host-only harness に閉じ込めるのが妥当。

### 7.2 不変条件テスト
冪等（`fmt(fmt(x))==fmt(x)`）と、GJF の 4 変換以外での有意トークン多重集合保存を property test に
（§9）。golden diff と併走させる。

---

## 8. jals 独自スタイルとの統合

GJF エンジン (`Break` の UNIFIED/INDEPENDENT/FORCED + BreakTag) は prettier group/fill の**上位互換**。
よって:

- **中核 IR/エンジンは GJF 版を採用**し、`jals-fmt` 全体の土台にする。
- **jals 独自の設定オプション**（brace-style, binop-separator, fn-params-layout, …）は、**L2 visitor が
  発行する Ops の差分**として表現する（同じエンジンの上で FillMode/Indent/Break の選択を変える）。
- **GJF 互換モード** = 「visitor を GJF 忠実発行に固定し、L0/L3/L4 の全パスを GJF 既定で有効化、
  独自オプションを GJF 値へ束縛」した特別プロファイル。設定値の集合ではなく**プロファイル**。
- text-normalization 族（リテラル書換え）は GJF 互換では無効、jals 独自モードでのみ有効。

この統合により、二重のエンジンを持たずに「GJF 100% 互換」と「jals 独自スタイル」を両立できる。

**戦略的含意（明示）**: 前段の考察は rustfmt 風の**設定可能**フォーマッタを志向していた。GJF 100%
互換を主目標に据える本書は、その優先順位を反転させる — **GJF の意見の固まったエンジンを丸ごと
移植して中核とし、rustfmt 風の設定オプション群は二次的な「jals ネイティブ・プロファイル」**
（同一エンジン上の Op 発行差分）に格下げする。つまり設定表面は設計の主駆動力ではなくなる。
ユーザの要求（GJF 完全一致）はこの反転を含意している。両者の唯一の共有基盤が GJF エンジンである。

---

## 9. 不変条件の再整理（GJF 互換下）

- **有意トークン保存**: GJF は 4 変換（import 整列 / 未使用 import 削除 / modifier 整列 / 長文字列
  再折り）でのみトークン列を変える。よって不変条件は「**この 4 変換を除き有意トークンの多重集合は
  保存**」へ緩める。import/modifier は多重集合保存（順序のみ）、未使用 import 削除は**部分集合**
  （削除を許す唯一の箇所）、StringWrapper は `+`/文字列片の多重集合を保存（再配置のみ）。
- **never-panic / lossless**: 未対応ノード・ERROR ノードは **verbatim 出力**へ fallback。最上位に
  fail-safe（出力再パースで新規 syntax error か有意トークン減があれば入力そのまま返す）。
- **冪等**: `computeBreaks` は greedy 純関数、StringWrapper は不動点検証つき。両者とも決定的。
  `fmt∘fmt=fmt` を第一級テストに。
- **コメント完全性**: 付着枠組みは「全コメント Tok を丁度 1 回描画」を debug-assert（biome 由来）。

---

## 10. Open questions / 要実測

- 配列初期化子の pack（一列/複数列/tabular）と arg/param の厳密な折り単位（GJF 実測）。
- `MAX_LINE_WIDTH` sentinel の厳密値（幅クランプ）。挙動には効かないが移植時に合わせる。
- `//` vs `/* */` の FillMode 割当の細部（trailing block の `breakAndIndentTrailingComment` 条件）。
- `normalize-parameter-comments` 相当を GJF が行うか（`/*name=*/` の空白正規化の有無）。
- text block 内部の再 indent を GJF が行うか（GJSG は「内容は保持」寄り、実測要）。
- StringWrapper が単一リテラルを新トークンへ割ることがあるか（一次情報未確認、既定は「やらない」想定）。

---

# Part II — 複数フォーマッタ完全互換（Spotless / Eclipse / IntelliJ）

Part I は GJF を単一ターゲットとして掘り下げた。本 Part は対象を Spotless / Eclipse JDT /
IntelliJ IDEA へ拡げ、「4 者すべてと完全互換な rule set」と「native config → jalsfmt.toml 自動生成」
の実現可能性・問題点・実装を洗う。**一次情報で確認した結論から述べる。**

## 11. 結論（先出し）

1. **4 者は 4 つの相互非互換なレイアウト解決アルゴリズムを持つ。** 単一エンジン + 設定値では
   どれか 1 つとしか bit 一致できない。**pluggable engine が必須。**

   | | 解決アルゴリズム | 内部 IR | 設定規模 | 入力空白依存 |
   |---|---|---|---|---|
   | **GJF** | greedy 単一パス `computeBreaks` | Doc/Break/Level 木 | 不可（固定 + AOSP） | なし（canonical） |
   | **Palantir** | **独自の探索/バックトラック**（GJF の Doc/Level/visitor は継承、break 決定を書換） | GJF 系 IR + 独自 break engine | ほぼ不可（Style 3 種 + formatJavadoc のみ） | なし（canonical） |
   | **Eclipse JDT** | **penalty 最小化探索** `WrapExecutor.findWraps`（memo 化、overflow→penalty 順） | 注釈付きトークン列 + `WrapPolicy` | **~400** | あり（空行保存等） |
   | **IntelliJ** | **greedy + 直近 wrap 候補へ rewind** `WrapProcessor` | `Block` 木 + Wrap/Indent/Align/Spacing | **~270** | **強くあり**（`keep*`） |
   | **Spotless** | —（**オーケストレータ**。上記へ委譲） | 線形 step パイプライン | build DSL | 委譲先次第 |

2. **Spotless は本設計の外側そのもの。** Spotless = `String→String` の step を**宣言順**に連ねる線形
   パイプライン。jals の 5 層パイプライン (L0–L4) は Spotless と**同種の構造物**。⇒ **外側のパイプ
   ライン構造は 4 者すべてに一般化する（○）。だが内側のレイアウト中核 (L1 engine + IR + L2 emission)
   は engine 固有で共有できない（✗）。**

3. **IntelliJ・Eclipse は入力空白に依存する（canonical でない）— 最深の問題。** GJF・Part I の設計・
   jals の不変条件はすべて **AST→canonical（空白盲目・冪等 by construction）**。IntelliJ は
   `keepLineBreaks`/`keepBlankLines`/`ij_java_keep_*` により**出力が入力の既存改行の関数**になる。
   意味的に同一で改行だけ違う 2 ファイルが別 byte になる。**jals の「有意トークン + 純レイアウト」
   モデルを根本から破る**。互換 engine は source 空白を読み**部分保持**する別モードが要る（§17）。

4. **意味論的操作は pure CST の外。** GJF/Spotless/IntelliJ の未使用 import 削除、IntelliJ の wildcard
   集約 (`class_count_to_use_import_on_demand`) は名前/型解決を要する。GJF の未使用 import 削除は
   名前ヒューリスティックで CST 完結（Part I R0.2）だが、wildcard 集約は「あるパッケージから N 個
   以上 import」の計数＝実 import 解決が要り、pure CST では不可。

5. **jalsfmt.toml 自動生成は「profile 選択 + option 透過」としてなら可能。統一スタイル言語としては
   不可能。** 生成 toml は `[compat] engine="eclipse"` + 復号オプションという**エンジン多重化器**で
   あり、単一の共通スタイル記述ではない。しかも (3)(4)(6) により「config を渡せば出力が決まる」保証
   自体が IntelliJ では崩れる。

6. **4 者すべて version 非安定。** 出力はリリース間で変わる。「100% 互換」は必ず**バージョン pin**
   付きで定義（Part I §7.1 を全ターゲットへ拡張）。

## 12. 各フォーマッタの実像（一次情報）

### 12.1 Spotless — オーケストレータ
- 単体 engine を持たず、Java では **GJF / Palantir / Eclipse JDT へ委譲**。設定は build DSL
  （Gradle `spotless{java{...}}` / Maven `<configuration>`）で、**独立した config ファイル形式は無い**
  （個々の step が外部ファイルを参照はする: `eclipse().configFile(...)`, `importOrderFile(...)`）。
- **委譲 step**: `googleJavaFormat(v)`(`.aosp()`/`.reflowLongStrings()`/`.reorderImports()`/
  `.skipJavadocFormatting()`) / `palantirJavaFormat(v)`(`.style("PALANTIR"|"GOOGLE"|"AOSP")` 既定
  PALANTIR、`.formatJavadoc(true)` は ≥2.39.0) / `eclipse(v).configFile(...)`。
- **Spotless 自前 step**: `importOrder(...)`/`importOrderFile(...)`（prefix グループ順、`''`=catch-all、
  `\#`=static 印。krasa/EclipseCodeFormatter 由来）/ `removeUnusedImports()`（既定 GJF 除去器、代替
  CleanThat。**意味論的**）/ `formatAnnotations()`（**~700 名の type-use 注釈ハードコード表**による
  ヒューリスティックで型注釈を型の行へ戻す。formatter の**後段**に置く想定）/ `licenseHeader(...)` /
  汎用 step（`trimTrailingWhitespace`/`endWithNewline`/`leadingTabsToSpaces(n)`・`leadingSpacesToTabs`
  〈旧 `indentWithSpaces/Tabs`〉/`replace`/`replaceRegex`/`custom`/`toggleOffOn`）。
- **パイプライン**: step は**宣言順**に text→text 適用（Maven README:「order matters, and this is
  good!」）。`toggleOffOn()` は `spotless:off/on` 区間を全 step から除外。`ratchetFrom` は git 差分に
  よる**ファイル選択**でレイアウト出力とは直交（互換性に無関係）。
- **帰結（確認済み）**: 「Spotless 互換」= **(a) 設定された委譲先 engine との互換 + (b) Spotless 自前
  step の再現 + (c) 同一 step 順序**。(a) がレイアウトの大半で、Spotless 自身は所有しない。

### 12.2 Eclipse JDT — penalty 最小化探索（旧 Scribe は 2015 に撤去）
- **重要**: 旧 `Scribe`/`Alignment`/`AlignmentException` バックトラック engine は **2015 年
  (Eclipse 4.5, Matela 書き直し) に JDT から完全撤去済**。現行は別物（旧 engine は CDT が fork 保持、
  歴史的参照のみ）。
- **現行パイプライン** (`DefaultCodeFormatter`): parse(DOM AST) → tokenize(`List<Token>`) →
  `SpacePreparator`(`insert_space_*`) → `LineBreaksPreparator`(改行/indent/brace/blank) →
  `OneLineEnforcer` → `CommentsPreparator`(block/line/javadoc/markdown) → `WrapPreparator`
  (`alignment_for_*`/`wrap_before_*`/`lineSplit` → per-token `WrapPolicy`) →
  `WrapExecutor.executeWraps()`（**探索**）→ `TextEditsBuilder`(`TextEdit[]`)。**再パースしない。**
- **折り決定（核心）**: memo 化 `findWraps(index, indent)` + 明示スタックの **penalty 最小化探索**。
  `lineOverflow=max(0, width-page_width)`。候補集合を評価し `isBetter = totalExtraPenalty < best ||
  (best 無し && wrapRequired)`、tie は totalPenalty。**overflow を先に最小化、次に wrap penalty**。
  GJF/prettier の greedy 一発とは別物 → **greedy Doc printer では bit 一致しない**。
- **config**: id 接頭辞 `org.eclipse.jdt.core.formatter.`、**~400 option**。families: indentation(11),
  brace_position(15; `end_of_line`/`next_line`/`next_line_shifted`/`next_line_on_wrap`), blank_lines,
  **insert_space(~183 文脈, 最大族)**, **alignment_for_*(53)**, wrap_before_*(~13), `lineSplit`(列上限,
  camelCase), comment.*(26; `comment.line_length` は別列上限), off/on(`use_on_off_tags`/`disabling_tag`
  =`@formatter:off`)。
- **alignment ビット符号化**: `alignment_for_*` は int 文字列。`M_FORCE=1`/`M_INDENT_ON_COLUMN=2`/
  `M_INDENT_BY_ONE=4`/`M_COMPACT=16`/`M_ONE_PER_LINE=48`/`M_NEXT_SHIFTED=64`/`M_NEXT_PER_LINE=80`
  (`SPLIT_MASK=0x70`)。これは**config 入力**にすぎず、実挙動は `WrapPreparator`→`WrapPolicy`
  (wrapMode/penaltyMultiplier/indentOnColumn) + 探索。**penalty 重み・列基準 indent は素の Wadler
  group/break に還元できない。**
- config ファイル: exported XML profile(`<profile kind="CodeFormatterProfile"><setting id=.. value=..>`)
  / `.settings/org.eclipse.jdt.core.prefs`。**JDT core は `.editorconfig` を読まない**（第三者プラグ
  インのみ、部分対応）。
- comment: 独立の `comment.*` 族(26) + `CommentsPreparator`/`CommentWrapExecutor`。

### 12.3 IntelliJ IDEA — Block 木 + greedy rewind、しかも入力空白依存
- **エンジン**: `FormattingModel` の `Block` 木。各 Block が `getIndent()`/`getWrap()`/`getAlignment()`、
  隣接 child 間に `getSpacing(c1,c2)`。解決は状態機械 `FormatProcessor`（WrapBlocks →
  **AdjustWhiteSpaces** → ExpandChildrenIndent → ApplyChanges）。折りは `WrapProcessor`: **greedy 左→右、
  行が右端超過で直近の保存 wrap 候補へ rewind** して break 挿入。`CHOP_DOWN_IF_LONG` は list 先頭
  (chop start) まで戻り全項目を折る。**大域最適でも一発 greedy でもない。**
- **Wrap 値語彙（要注意・反直感）**: editorconfig token → 意味: `off`=DoNotWrap / `normal`=WrapIfLong /
  **`split_into_lines`=Wrap Always** / **`on_every_item`=Chop Down If Long**（この 2 つを取り違え易い）。
- **brace 値語彙**: `end_of_line`/`next_line`/`next_line_if_wrapped`/**`whitesmiths`**(=NextLineShifted)/
  **`gnu`**(=NextLineShifted2)。（`next_line_shifted*` ではない。）
- **入力空白依存（最重要）**: `Spacing` の `keepLineBreaks`/`keepBlankLines` と多数の `ij_java_keep_*`
  により**既存の改行/空行を設定上限まで保持**。⇒ 出力が入力空白の関数（canonical でない）。
- **config**: `.editorconfig` の `ij_java_*`(**~250–270**; spacing~43 / wrapping~28 `*_wrap` /
  blank_lines~16 / brace 4 / keep~14 / align~23 / force-braces `*_brace_force`=`never`/`if_multiline`/
  `always` / imports)。または XML scheme `.idea/codeStyles/Project.xml`（enum は**生 int**、editorconfig
  は**名前 token**、両変換表が要る）。各設定は **内部 int/bool・XML 生 int・editorconfig token の 3 表現**。
- **意味論依存**: wildcard 集約閾値・optimize-imports は classpath 解決要。version 間で block builder /
  既定が変化（records/deconstruction で新 wrap/align 追加）。

### 12.4 Palantir — GJF フォークだが break エンジンは別物
- **config は実質なし**（GJF と同じ非設定思想を javadoc に逐語継承）。`JavaFormatterOptions` の全設定は
  `Style`(PALANTIR=indent×2/120 桁, GOOGLE=×1/100, AOSP=×2/100) と `boolean formatJavadoc`(既定 false)
  の 2 つだけ。列上限・indent は Style の従属値で個別設定不可。CLI flag は GJF と同型
  (`--palantir`/`--aosp`/`--skip-*`)、`--format-javadoc` は無く API/plugin/Spotless 経由のみ。既定は
  3 層（ライブラリ/生 CLI=GOOGLE、製品面〈IntelliJ/Gradle plugin/Spotless〉=PALANTIR）。設定**ファイル**
  は持たない（GJF 同様）。
- **engine は別物（重要な訂正）**: GJF の Doc/Level/Op scaffolding・visitor 構造・前後パスは継承するが、
  **break 決定核を書き換えている**: `BreakBehaviour`(Level 毎に breakThisLevel /
  preferBreakingLastInnerLevel / inlineSuffix / breakOnlyIfInnerLevelsThenFitOnOneLine) +
  `LastLevelBreakability` + `PartialInlineability`(過長 Level の **prefix を同一行へ部分インライン**＝
  method-chain の特徴挙動) + `Obs`(**複数仮説を explore してバックトラック**、行数で優劣判定)。GJF の
  greedy 一発とは別。⇒ **Style 定数を GJF engine に渡しても Palantir 出力にはならない。独立 engine の
  移植が要る（実質 4 つ目のエンジン）。** ただし canonical（空白盲目）なので不変条件は canonical モード
  （§17）で GJF と同じ。

## 13. この構造化は適用できるか — 層別判定

| 層 | 4 者への一般化 | 判定 |
|---|---|---|
| **外側パイプライン (L0–L4)** | Spotless そのもの。全 engine が pre-pass→engine→comments→post に収まる | ○ 一般化 |
| **L1 レイアウトエンジン** | GJF greedy / Eclipse penalty 探索 / IntelliJ greedy-rewind は**別アルゴリズム** | ✗ **pluggable 必須** |
| **IR (Doc-IR)** | GJF=Doc 木 / Eclipse=注釈トークン列 / IntelliJ=Block 木。共有不能 | ✗ engine 固有 |
| **L0 import/modifier 整列** | 順序規則は engine 差だが sequence-reordering の枠は共通 | ○ 枠共有・規則パラメータ |
| **L2 emission** | engine ごとに break/space/indent の置き方が違う | ✗ engine 固有 |
| **L3 comments** | 付着枠組みは共有可、placement 規則は engine 差 | △ 枠共有・規則差 |
| **L4 汎用 step / finalize** | trim / final-newline / off-on / indent 変換は engine 非依存 | ○ 完全共有（= Spotless 汎用 step） |
| **意味論 step（未使用 import / wildcard 集約）** | pure CST 外（GJF 版のみ名前ヒューリスティックで CST 内） | ✗/△ jals-hir or ヒューリスティック |

**総括**: 「この構造化を適用できるか」への答えは —— **外側の合成パイプライン (L0/L4/step 順)、
sequence-reordering の枠、汎用 step は 4 者に一様適用できる（○）。だがレイアウトの中核 (L1 engine +
IR + L2 emission) は engine 固有で共有不能であり、4 つの engine を pluggable に持つほか無い（✗）。
つまり `jals-fmt` は「GJF/Eclipse/IntelliJ の各 engine を移植して束ねる Spotless 様マルチエンジン
パイプライン」になる。**

## 14. アーキテクチャ: マルチエンジン・パイプライン

```
              CompatProfile ( gjf | palantir | eclipse | intellij | jals-native )
                    │  engine + option 復号 + 有効パス を選ぶ
 ┌──────────────────┼───────────────────────────────────────────────┐
 │ 共有 framework (engine 非依存)                                     │
 │  rowan CST 取得 / config 発見+Reify / コメント付着プラグイン枠     │
 │  汎用 step(trim, final-newline, off-on, indent 変換) / invariant   │
 └────────┬───────────────────────────────────┬──────────────────────┘
          ▼                                     ▼
   ┌ trait LayoutEngine ┐             ┌ trait ConfigImporter ┐
   │ GjfEngine (Doc/greedy)│          │ Eclipse prefs/XML → toml │
   │ EclipseEngine (探索)  │          │ IntelliJ editorconfig/XML│
   │ IntellijEngine (Block)│          │ Spotless build DSL → pipe│
   │ PalantirEngine(探索/BT)│         │ (toml 不在 → 自動生成)   │
   └───────────────────────┘          └──────────────────────────┘
```

- **`LayoutEngine` trait**: `fn format(&self, cst: &SyntaxNode, opts: &Self::Opts, src: &str) -> String`。
  4 実装。GJF/Palantir は内部 IR(Doc) を共有、Eclipse/IntelliJ は各自の内部表現。`src` を渡すのは
  **IntelliJ/Eclipse が入力空白を要する**ため（GJF は無視）。
- **`CompatProfile`**: どの engine・どの option 群・どの pass を有効化するかの束。GJF 互換 = Part I の
  プロファイル。
- **共有 framework**: CST・config 発見・**汎用 step**（Spotless の generic step と同一物）・off/on 領域・
  不変条件ハーネス。ここは 4 者で完全共有。
- **`ConfigImporter` trait**: native config → jalsfmt.toml（§15）。

## 15. jalsfmt.toml 自動生成

**方式: profile-selector + option 透過**（統一スタイル言語ではない）。生成 toml の形（例, Eclipse）:

```toml
[compat]
engine  = "eclipse"          # gjf | palantir | eclipse | intellij
version = "4.31"             # bit 互換は version pin 必須
source  = ".settings/org.eclipse.jdt.core.prefs"   # 由来（再生成の追跡）

[compat.eclipse]             # engine 固有 option を透過（~400 のうち非既定のみ）
"org.eclipse.jdt.core.formatter.lineSplit" = 120
"org.eclipse.jdt.core.formatter.brace_position_for_type_declaration" = "next_line"
```

**検出と生成（jalsfmt.toml 不在時）**: jals-config の発見機構を拡張し優先順で走査:
1. `jalsfmt.toml`（あれば何もしない）
2. `.editorconfig` に `ij_java_*` → `engine="intellij"`
3. `.settings/org.eclipse.jdt.core.prefs` / exported formatter XML → `engine="eclipse"`
4. `build.gradle(.kts)`/`pom.xml` の spotless ブロック → spotless パイプライン写像（委譲先 engine +
   step 列 + 順序）
5. 何も無ければ GJF 既定 or jals-native

**限界（明示すべき）**:
- **P-gen-1 統一不能**: engine が違えば同一 toml で bit 一致は出せない。toml は engine 多重化器。
- **P-gen-2 空白依存**: IntelliJ は config だけで出力が決まらない（入力空白の関数）。「config→出力」の
  全単射が無く、生成 toml でも入力次第で乖離。元 IDE と一致するには入力空白保持 engine が要る。
- **P-gen-3 意味論**: 未使用 import 削除・wildcard 集約は toml で表せない挙動。jals-hir か名前ヒュー
  リスティックが要り、toml には有効/無効フラグしか置けない。
- **P-gen-4 Spotless DSL**: build.gradle(Groovy/Kotlin)/pom.xml の spotless ブロックはデータでなく
  コード。完全解析は不可能、よくある形のパターン抽出に留め、未対応は警告。
- **P-gen-5 語彙衝突**: Eclipse brace `next_line_shifted` と IntelliJ `whitesmiths`/`gnu`、wrap token の
  反直感（`split_into_lines`=always）等、native 語彙を機械的に取り違えない写像表が要る。
- **P-gen-6 option 網羅**: Eclipse ~400 / IntelliJ ~270 の**非既定オプションのみ**透過し、既定は engine
  側の既定（version 込みで固定）に委ねる。

## 16. 実装の現実的段階（先に価値が出る順 / 難度昇順）

1. **GJF engine**（Part I）— canonical・設定不可・名前ヒューリスティック import。土台。
2. **Palantir engine** — GJF の IR/visitor/前後パスは共有できるが、**break 決定は独自の探索/バック
   トラック engine（`BreakBehaviour`/`PartialInlineability`/`Obs`）を別途移植**（GJF engine の折り差分
   では不可）。canonical なので土台側だが、実質 4 つ目のエンジン。別 golden 要。
3. **汎用 step 層 + Spotless パイプライン写像** — engine 非依存。委譲さえ揃えば Spotless 互換が最短。
4. **Eclipse engine** — penalty 探索 + ~400 option 復号 + WrapPolicy。大工数だが canonical 寄り。
5. **IntelliJ engine** — Block 木 + greedy-rewind + **入力空白保持** + ~270 setting + 3 表現変換。
   最難関（whitespace 依存が jals の不変条件モデルを最も強く破る）。

各段は golden harness（実ツール × コーパスの byte diff、Part I §7.1）で検証。

## 17. 不変条件の再整理（マルチエンジン下）

**2 つの整形モードを明示的に区別する:**

- **canonical モード**（GJF / Palantir / jals-native）: AST→出力、空白盲目、冪等 by construction、
  有意トークン多重集合保存（engine の変換を除く）。Part I §9 の不変条件がそのまま成立。
- **whitespace-retaining モード**（Eclipse / IntelliJ）: 入力空白を部分保持。**「有意トークン + 純
  canonical レイアウト」不変条件は成立しない**。代わりに (a) 冪等 = 不動点、(b) 有意トークン多重
  集合保存（engine の変換を除く）、(c) 保持は「既存空白を設定上限内へ clamp する決定的関数」、を
  不変条件とする。

`never-panic` / `verbatim fallback` / `off-on` 領域は全 engine 共通。golden 検証は各 engine を pin した
実ツールに対して byte 一致率を測り、canonical モードは `fmt∘fmt=fmt`、whitespace-retaining モードは
「実ツール出力を再整形して不動点」を追加検証する。

---

## 18. 実現可能性の天井（ターゲット別）と「移植 vs 委譲」

「完全互換」と言うが、**byte 完全一致の到達可能性はターゲットで大きく違う**。一次調査が示す天井を
率直に順位づける:

| ターゲット | byte 完全一致の天井 | 理由 |
|---|---|---|
| **GJF** | **到達可能**（version pin 付き） | canonical・空白盲目。config→出力が全単射 |
| **Palantir** | **到達可能**（version pin） | canonical だが GJF とは**別の探索/バックトラック engine**の移植が要る（Style フラグでは不可） |
| **Eclipse** | **原理的に到達可能・高コスト** | ~400 option の penalty 探索 engine の移植。canonical 寄りだが空行保存あり |
| **IntelliJ** | **単体ツールでは完全到達不可** | 出力が**入力空白の関数**（AST+config だけで決まらない）＋ wildcard 集約/未使用 import が classpath 依存＋ version 非安定。**layout 近似は可、任意手書き入力での byte 完全一致は不可** |

⇒ 「4 者すべてと 100% 一致」を額面通り約束はできない。**IntelliJ は "layout-exact 近似" が現実的
上限**であることを設計に明記する。

**移植 (port) vs 委譲 (delegate) — 実装の 2 経路。** 唯一のオーケストレータ Spotless は互換性を
**実フォーマッタへの委譲**で達成し、engine を再実装していない。jals も同じ選択肢を持つ:

| 方式 | 内容 | 長所 | 短所 |
|---|---|---|---|
| **port** | engine を Rust 移植 | 単一バイナリ・wasm 可・JVM 不要・LSP 常駐で高速 | engine あたり多年工数、version 追随が永続コスト |
| **delegate** | 実ツール(JVM 等)を子プロセス実行し出力採用 | **即座に真の 100% 一致**、保守は version pin のみ | JVM 依存、wasm 不可、起動コスト、CST 非共有 |

現実解は**ハイブリッド**: GJF/Palantir は port（canonical・wasm playground でも動く土台。ただし
Palantir は GJF とは別 break engine の移植）、Eclipse/IntelliJ は
当面 **delegate**（`jals-cli`/`jals-lsp` の host 側子プロセス、golden と同経路）とし、需要が固まった
engine から段階的に port。`LayoutEngine` trait は port 実装と delegate 実装（`ExternalToolEngine`）を
**同一 interface 裏に置ける**ようにする。

**エントリポイント**: 現行公開 API は `FormatOutput::format_source(src, &Config)`（`jals-cli`/
`jals-lsp`/`jals-playground` が依存）。マルチエンジン化は `Config` に **profile 判別子**を足すか
`format_source(src, &Profile)` へ拡張（`Profile` が engine + options を保持）。既存シグネチャは
`Profile::JalsNative(Config)` への薄いラッパとして温存できる。

## 19. クレート境界と no_std（ワークスペース規約との整合）

`jals-fmt` は**ポータブルな `no_std` ドメインクレート**（CLAUDE.md:「do not add host filesystem
APIs」）。各部品を規約に沿って配置する:

- **`jals-fmt`（portable, no_std+alloc, wasm）**: `LayoutEngine` trait と **port 実装**（GjfEngine 等）、
  IR、汎用 step、invariant。**host FS/JVM に触れない。**
- **config 発見（どのファイルが在るか）**: host I/O ゆえ **`jals-cli`/`jals-lsp`** が担い、
  `jals-storage` の `ProjectView` 経由でバイト列を得る（生 `std::path`/`std::fs` は native adapter のみ）。
- **`ConfigImporter`（バイト列→jalsfmt.toml）**: **パース自体は portable に書ける**が、依存
  （XML/editorconfig/DSL パーサ）は **`no_std+alloc+wasm` を満たす**か `native`/`std` feature で host
  側に gate する。Groovy/Kotlin の spotless ブロック解析は現実的に host 専用。
- **`ExternalToolEngine`（delegate）**: 子プロセス実行ゆえ **host 専用**（`jals-cli`/`jals-lsp`、
  `jals-exec` の blocking pool 経由）。wasm ビルドからは feature 除外。
- **golden harness（実ツール × コーパス）**: JVM 依存。**`jals-tests` の host-only harness**に隔離
  （Part I §7.1）。通常ビルドに JVM を持ち込まない。

## 20. CLAUDE.md 不変条件の改訂は利用者判断

CLAUDE.md は**ハード不変条件**として明記している:
> *Formatting is idempotent and preserves the significant token sequence unless an explicitly
> configured text-normalization rule applies.*

Eclipse/IntelliJ 互換が要求する **whitespace-retaining モード**、および import 整列/未使用 import
削除/wildcard 集約/modifier 整列は、この「有意トークン列を保存」不変条件を**破る**（§17）。これは
§17 で静かに再定義して済む話ではなく、**ワークスペースの中核契約（documented invariant）を編集する
意思決定**であり、利用者が明示的に判断すべき事項である。

- (A) 不変条件を「**canonical モードでのみ**有意トークン保存」と限定し、compat engine を例外として
  文書化（CLAUDE.md 改訂）。
- (B) jals-fmt 本体は canonical（GJF/Palantir/jals-native）に限定し、Eclipse/IntelliJ は **delegate
  専用**（jals は再整形せず実ツール出力を採用）とし、**jals 本体の不変条件を保つ**。

推奨は (B) を既定とし、port を進める engine のみ (A) の例外を個別追記。**どちらを採るかは実装着手前に
確定すべき。**

---

## 付録 A: config ファイル形式リファレンス（ConfigImporter 実装用）

§15 の自動生成/検出を実装可能にするための、各 config ファイルの**具体形式・検出シグネチャ・実例・
パーサの罠**。すべて実ファイル/一次仕様で検証済み。

### A.1 検出の優先順とシグネチャ

| 優先 | ファイル | 検出シグネチャ（内容ベース） | → engine |
|---|---|---|---|
| 1 | `jalsfmt.toml` | 存在 | 何もしない |
| 2 | `.editorconfig`（`ij_` 系キーあり） | `ij_java_*` / `ij_*` キー、または `[*.java]` 節 | intellij |
| 3 | `.idea/codeStyles/Project.xml` | `<component name="ProjectCodeStyleConfiguration">` + `<code_scheme name="Project">` | intellij |
| 4 | exported IDE scheme `*.xml` | ルートが `<code_scheme name="...">`（`<component>` 親なし） | intellij |
| 5 | Eclipse XML profile `*.xml` | `<profile kind="CodeFormatterProfile">` / `<setting id="org.eclipse.jdt.core.formatter.` | eclipse |
| 6 | `.settings/org.eclipse.jdt.core.prefs` | `org.eclipse.jdt.core.formatter.` 行 + `eclipse.preferences.version=` | eclipse |
| 7 | `build.gradle(.kts)` / `pom.xml` | `com.diffplug.spotless` / `spotless {` / `spotless-maven-plugin` | spotless（委譲先を追う） |
| 8 | 上記なし | — | gjf 既定 / jals-native |

名前でなく**内容で判定**する（Eclipse XML も IntelliJ exported scheme も任意ファイル名）。`.editorconfig`
の `ij_` 接頭辞は IntelliJ 固有の強いマーカ。GJF は**設定ファイルが存在しない**（A.7）ので直接検出不能。

### A.2 EditorConfig（共通土台 — spec.editorconfig.org）

INI 風。IntelliJ が主用途だが標準規格。パーサが守る点:
- ファイル名は厳密に `.editorconfig`、UTF-8、LF/CRLF。行頭 `#`/`;` のみコメント（行途中は文字）。
- 先頭 preamble に `root = true` があれば親方向の探索を打ち切る。節見出しは glob（`[*]`/`[*.java]`/
  `[{a,b}.ext]`/`[lib/**.js]`）。**キーは大小無視（小文字化）、glob は大小区別**。
- カスケード: 遠い→近いの順に適用し**近いファイルが上書き**、同一ファイル内は**後の節が優先**。
- glob: `*`(=`/`以外) / `**`(=`/`含む) / `?` / `[seq]` / `[!seq]` / `{s1,s2}` / `{n1..n2}`、`\` エスケープ。
  glob に `/` を含めば当該 `.editorconfig` のディレクトリ相対。
- **universal 7 プロパティ**: `indent_style`(tab|space) / `indent_size`(正整数|tab) / `tab_width` /
  `end_of_line`(lf|cr|crlf) / `charset` / `trim_trailing_whitespace` / `insert_final_newline`。
- **罠**: `max_line_length` は core 外（限定サポート）。値域は**正整数 | `unset`** で、**`off` は spec 外**
  （Prettier 拡張）。`unset` は上位設定の効果を打ち消す。未知キーは plugin なら**無視**（エラーにしない）。

```ini
root = true
[*]
end_of_line = lf
insert_final_newline = true
[*.java]
indent_style = space
indent_size = 4
max_line_length = 100
```

### A.3 Eclipse JDT

**A.3.1 exported XML profile**（現行 `version="23"`、`ProfileVersionerCore.CURRENT_VERSION`）:
```xml
<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<profiles version="23">
<profile kind="CodeFormatterProfile" name="Eclipse" version="23">
<setting id="org.eclipse.jdt.core.formatter.tabulation.char" value="space"/>
<setting id="org.eclipse.jdt.core.formatter.tabulation.size" value="4"/>
<setting id="org.eclipse.jdt.core.formatter.lineSplit" value="120"/>
<setting id="org.eclipse.jdt.core.formatter.brace_position_for_method_declaration" value="end_of_line"/>
<setting id="org.eclipse.jdt.core.formatter.insert_space_after_comma_in_annotation" value="insert"/>
<setting id="org.eclipse.jdt.core.formatter.alignment_for_additive_operator" value="16"/>
</profile>
</profiles>
```
- `version` は 1..23 の履歴値をとりうる（Google 提供は 13）。1 ファイルに複数 `<profile>` 可（Export All）。
- 値語彙: bool `true`/`false`、insert 系は `insert` / **`do not insert`**（小文字・スペース 2 個）、
  `tabulation.char`=`tab`/`space`/`mixed`、brace=`end_of_line`/`next_line`/`next_line_shifted`/
  `next_line_on_wrap`。
- **`alignment_for_*` は 10 進整数の文字列だが中身はビットマスク**（`M_FORCE=1`/`M_INDENT_ON_COLUMN=2`/
  `M_INDENT_BY_ONE=4`/split `16|48|64|80`、`SPLIT_MASK=0x70`）。`16` と `49` はフラグ違い → **ビット分解**
  する（不透明 id 扱い禁止）。`2147483647`(=Int.MAX) は「無効/never」の sentinel。

**A.3.2 `.settings/org.eclipse.jdt.core.prefs`**（`java.util.Properties`、ISO-8859-1、`#`/`!` コメント、
`\uXXXX`/`\:`/`\=` エスケープ）:
```properties
eclipse.preferences.version=1
org.eclipse.jdt.core.compiler.compliance=21
org.eclipse.jdt.core.formatter.tabulation.size=4
org.eclipse.jdt.core.formatter.lineSplit=120
org.eclipse.jdt.core.formatter.alignment_for_additive_operator=16
```
- `formatter.` 以外の名前空間（`compiler.` 等）が同居する。内容マーカは `org.eclipse.jdt.core.formatter.`。
  `eclipse.preferences.version=1` は prefs ストア版であって profile の 23 とは無関係。

### A.4 IntelliJ IDEA

**A.4.1 `.editorconfig`（`ij_java_*`）** — A.2 に加え IntelliJ 接頭辞:
無印=標準キー / `ij_`=全言語 / `ij_any_`=多言語共通 / `ij_java_`=Java 固有。
```ini
[*.java]
max_line_length = 100
ij_continuation_indent_size = 4
ij_formatter_off_tag = @formatter:off
ij_java_call_parameters_wrap = normal
ij_java_binary_operation_wrap = normal
ij_java_method_brace_style = end_of_line
ij_java_if_brace_force = never
```

**A.4.2 enum 変換表（最重要の罠）** — 同じ enum を **XML=生 int / editorconfig=名前** で二重表現し、
対応が非直感。しかも **プロパティごとに別の表**（wrap ≠ brace ≠ force-braces。1 つの表を使い回さない）:

brace-style (`*_BRACE_STYLE`):
| XML int | editorconfig |
|---|---|
| 1 | `end_of_line` |
| 2 | `next_line` |
| 5 | `next_line_if_wrapped` |
| 3 | **`whitesmiths`** |
| 4 | **`gnu`** |

wrap (`*_WRAP`):
| XML int | editorconfig |
|---|---|
| 0 | `off` |
| 1 | `normal` |
| 2 | **`split_into_lines`**(=Wrap Always) |
| 4 | `on_every_item`(=Chop Down) |
| 5 | `on_every_item`（**ロス**: 読み戻しは 4） |

force-braces (`IF_BRACE_FORCE` 等): `0`=しない … `3`=always（brace-style とは別 enum）。

**A.4.3 `.idea/codeStyles/codeStyleConfig.xml`**:
```xml
<component name="ProjectCodeStyleConfiguration">
  <state><option name="USE_PER_PROJECT_SETTINGS" value="true" /></state>
</component>
```
**A.4.4 `.idea/codeStyles/Project.xml`**（enum は**生 int**、scheme 名は `"Project"`、`version="173"`）:
```xml
<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173">
    <option name="RIGHT_MARGIN" value="120" />
    <option name="CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND" value="99" />
    <JavaCodeStyleSettings>
      <option name="IMPORT_LAYOUT_TABLE">
        <value>
          <package name="" withSubpackages="true" static="false" module="true" />
          <package name="java" withSubpackages="true" static="false" />
          <emptyLine />
          <package name="" withSubpackages="true" static="true" />
        </value>
      </option>
    </JavaCodeStyleSettings>
    <codeStyleSettings language="JAVA">
      <option name="IF_BRACE_FORCE" value="3" />
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
        <option name="CONTINUATION_INDENT_SIZE" value="4" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>
```
- 構造: `RIGHT_MARGIN`/`*_COUNT_TO_USE_IMPORT_ON_DEMAND` は `<code_scheme>` 直下、import 政策は
  `<JavaCodeStyleSettings>`、空白/wrap/indent は `<codeStyleSettings language="JAVA">`＋`<indentOptions>`。
- `PackageEntryTable`(`IMPORT_LAYOUT_TABLE`/`PACKAGES_TO_USE_IMPORT_ON_DEMAND`)=`<value>` 内に
  `<package name withSubpackages static [module]/>` と `<emptyLine/>` の順序付き列。空表は `<value/>`。
- 言語スコープを持つ要素は 2 形。`<codeStyleSettings language="…">` と、`<…CodeStyleSettings>` 兄弟
  (`<KotlinCodeStyleSettings>`/`<XmlCodeStyleSettings>`/…)。どの言語も **同一の UPPER_SNAKE option 語彙**
  を使い回すので、Java 以外のこの 2 形は**部分木ごと捨てる**（読むと Java の値を上書きする）。逆に
  top-level は言語スコープではないので Java として読む（A.4.5）→ Java 許可リストではなく他言語拒否リスト。

**A.4.5 exported IDE scheme**（Settings→Code Style→Export）: ルートが `<code_scheme name="...">` 直で
`<component>` ラッパも `codeStyleConfig.xml` 相棒も無い。`version` 属性は任意（旧 export には無い）。
旧 Google scheme は一部 import option を top-level に置く → パーサは `<JavaCodeStyleSettings>` 内/外
どちらの `<option>` も受理する。

### A.5 Spotless（build DSL — 値抽出はヒューリスティック）

> **重要**: Gradle(Groovy/Kotlin)・pom.xml の値は**コード**。version/path が変数・補間 `${...}`・計算値で
> ありうる。**「Spotless の存在検出」は確実だが「値の抽出」は best-effort**（未解決は警告）。

Gradle Groovy:
```gradle
spotless {
  java {
    googleJavaFormat('1.28.0').aosp().reflowLongStrings().skipJavadocFormatting()
    // eclipse('4.26').configFile('eclipse-formatter.xml')
    importOrder('java', 'javax', '', '\\#')   // '' = catch-all, '\\#' = static
    removeUnusedImports()
    formatAnnotations()
    licenseHeader('/* (C) $YEAR */')
    trimTrailingWhitespace(); endWithNewline(); toggleOffOn()
  }
}
```
Gradle Kotlin: 型安全アクセサ形（`spotless { java { googleJavaFormat("1.28.0").aosp() ... } }`、`$` は `\$`）
と、間接適用時の `configure<com.diffplug.gradle.spotless.SpotlessExtension> { ... }` 形の 2 通り。
Maven `pom.xml`:
```xml
<plugin>
  <groupId>com.diffplug.spotless</groupId><artifactId>spotless-maven-plugin</artifactId>
  <version>${spotless.version}</version>
  <configuration><java>
    <googleJavaFormat><version>1.28.0</version><style>GOOGLE</style></googleJavaFormat>
    <importOrder><order>java|javax,org,com,,\#</order></importOrder>
    <removeUnusedImports/><trimTrailingWhitespace/><endWithNewline/>
  </java></configuration>
</plugin>
```
- 検出: `com.diffplug.spotless`（plugins/apply）・`spotless {`・`spotless-maven-plugin`。step は宣言順。
- Spotless 互換 = §12.1 の (a) 委譲先 engine + (b) 自前 step 再現 + (c) 同一順序。委譲先は上の
  `googleJavaFormat`/`eclipse().configFile`/`palantirJavaFormat` から判定し対応 engine を選ぶ。

### A.6 補助ファイル

**`.importorder`**（`importOrderFile`/Maven `<file>`。`.properties` 風、**整数キー=順序**、値=prefix）:
```properties
#Organize Import Order
0=\#
1=
2=javax
3=java
4=com.myTeam
```
- **キーの数値順で整列**（ファイルの行順ではない）。**空値=catch-all**（「他に一致しない全 import」。
  空行区切りではない）。**`\#`=static import 群**（`.properties` では行頭 `#` がコメントなので `\` で退避）。
- **`\#` のバックスラッシュ数がホストで違う**: `.importorder`=`\#` / Groovy=`'\\#'` / Kotlin=`"\\#"` /
  Maven XML=`\#`。
**licenseHeader**: `$YEAR`(=現在年、既存年があれば範囲化)。Java step は既定 delimiter
（`(package|import|public|class|module) ` 相当）を持つのでヘッダ本文のみで可。

### A.7 google-java-format / palantir-java-format
両者とも**独自 config ファイルは存在しない**（意図的に設定不可）。
- **GJF**: スタイルは Google(2) / AOSP(4) の 2 変種のみで CLI `--aosp` かビルドツール option で選ぶ。
- **Palantir**: 設定は `Style`(PALANTIR/GOOGLE/AOSP) + `formatJavadoc` のみ。CLI `--palantir`/`--aosp`、
  Spotless `.style(...)`/`.formatJavadoc(true)`、plugin で選ぶ。**engine は GJF とは別**（§12.4）。

⇒ どちらも**ファイルシステムからの直接検出は不可**、Spotless step / Maven fmt plugin / IntelliJ
プラグイン痕跡 / 明示指定から間接的に推定するのみ。

### A.8 パーサの罠まとめ
1. IntelliJ の生 int enum は**プロパティ別**（wrap/brace/force-braces で表が違う）。1 表使い回し禁止。
2. Eclipse `alignment_for_*` は**ビットマスク**（不透明 id でない）。
3. editorconfig `max_line_length=off` は spec 外。値域は正整数|`unset`。
4. `\#` のエスケープ数がホスト形式依存。
5. Spotless は**コード**。存在検出は確実、値抽出はヒューリスティック（未解決警告）。
6. Eclipse profile version=23、IntelliJ scheme version=173（いずれも version pin と併記）。

---

## 付録 B: 出典ファイル
`core/src/main/java/com/google/googlejavaformat/` 配下: `Doc.java`, `Op.java`, `OpsBuilder.java`,
`DocBuilder.java`, `Indent.java`, `java/Formatter.java`, `java/JavaFormatterOptions.java`,
`java/JavaInputAstVisitor.java`, `java/JavaInput.java`, `java/ImportOrderer.java`,
`java/ModifierOrderer.java`, `java/RemoveUnusedImports.java`, `java/StringWrapper.java`,
`java/CommandLineOptions.java`, `java/javadoc/JavadocFormatter.java`。
Google Java Style Guide: https://google.github.io/styleguide/javaguide.html。

Part II 出典:
- **Eclipse JDT** (`github.com/eclipse-jdt/eclipse.jdt.core` @ master):
  `.../internal/formatter/DefaultCodeFormatter.java`, `.../linewrap/{WrapExecutor,WrapPreparator,
  CommentWrapExecutor}.java`, `.../formatter/DefaultCodeFormatterConstants.java`,
  `.../internal/formatter/DefaultCodeFormatterOptions.java`(Alignment ビット),
  `{SpacePreparator,LineBreaksPreparator,CommentsPreparator,OneLineEnforcer}.java`;
  `eclipse.jdt.ui` の `ProfileStore.java`(XML profile)。
- **IntelliJ** (`github.com/JetBrains/intellij-community` @ master):
  `platform/code-style-api/src/com/intellij/formatting/{WrapType,Wrap,Indent,Spacing,Alignment}.java`,
  `platform/code-style-impl/.../engine/{AdjustWhiteSpacesState,WrapProcessor}.java`,
  `platform/code-style-api/.../CommonCodeStyleSettings.java`,
  `java/java-frontback-impl/.../JavaCodeStyleSettings.java`,
  `.../properties/{WrappingAccessor,BraceStyleAccessor}.java`;
  docs: plugins.jetbrains.com/docs/intellij/code-formatting.html, jetbrains.com/help/idea/editorconfig.html。
- **Spotless** (`github.com/diffplug/spotless` @ main): `plugin-gradle/README.md`, `plugin-maven/README.md`,
  `lib/.../java/{GoogleJavaFormatStep,PalantirJavaFormatStep,RemoveUnusedImportsStep,ImportOrderStep,
  FormatAnnotationsStep}.java`。
