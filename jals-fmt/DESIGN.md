# jals-fmt 設計: 単一エンジン + rule による複数フォーマッタ近似

> **方針（本書の前提）**: `jals-fmt` は**レイアウトエンジンを 1 つだけ持つ**。google-java-format
> (GJF) の IR とレイアウト解決を土台にした単一エンジンを実装し、**Spotless / Eclipse JDT /
> IntelliJ IDEA / Palantir への追従は「rule による調整」の範囲で行う**。エンジンを複数持って
> 各社の解決アルゴリズムを移植することはしない。すなわち **byte 完全一致を全ターゲットに約束せず、
> 単一エンジンの一貫性を優先する**（優先順位の明示は §11、精度の階層と恒久差分は §18）。
>
> 目的: 既存 Java フォーマッタの native config が存在し `jalsfmt.toml` が無い場合に**互換 toml を
> 自動生成**し、その rule 束で「実用上ほぼ同じ見た目」を出せる基盤にする。本書は必要な rule/behavior
> のリストと、それらが**この構造化 (5 層パイプライン + Doc-IR) に載るか**の判定、**エンジンの可変点
> (seam)**、および Rust 表現を定める。
>
> **本書の構成:**
> - **Part I (§0–§10)**: 単一エンジンの基準系。GJF を一次情報で深掘りし、IR/エンジン/rule/不変条件を
>   定義する。既定プロファイル `gjf` はこのエンジンの**ネイティブ意味論そのもの**であり、byte 一致を
>   狙える唯一のターゲットである。§8 が **rule による調整の可変点 (seam)** を定義する。
> - **Part II (§11–§20 + 付録)**: 対象を Spotless / Eclipse / IntelliJ / Palantir へ拡張。**4 者が
>   相互非互換な 4 つのエンジンである**事実を一次情報で確認したうえで、**それらを移植せず rule で
>   近似する**という判断と、その帰結（精度の階層、恒久差分の列挙、config 自動生成の限界）を洗う。
>
> 全フォーマッタの内部仕様は一次情報 (各 OSS の `master` ソース + 公式仕様) から確認した。GJF の
> ソース参照は `core/src/main/java/com/google/googlejavaformat/` 配下のパスで示す。**Part II の
> 「各エンジンの実像」(§12) は移植しないと決めた後も残す** — 妥協が無知ではなく既知の上での判断で
> あることの根拠であり、rule で近似できる範囲の境界そのものだからである。

---

# Part I — 単一エンジンの基準系: google-java-format

## 0. スコープと非スコープ

対象: GJF CLI の既定 `format` が行う整形の再現（GOOGLE スタイル / AOSP バリアント）を**単一エンジンの
基準系**として据えること。Part I の記述は「GJF の意味論をエンジンのネイティブ挙動として実装する」
という意味であり、rule を既定 (`gjf` プロファイル) から動かせば出力は当然 GJF と離れる。

**先に確定すべき事実 — GJF は設定不可。** README 明言:「フォーマットアルゴリズムに設定可能性
は無い。単一フォーマットへ統一するための意図的な設計判断である」。バリアントは AOSP のみで、
差は**indent 倍率だけ**（`JavaFormatterOptions`: `GOOGLE(1)` / `AOSP(2)` → block 2/4, 継続 4/8,
列上限は共に 100）。

**帰結（本設計の背骨）:** GJF 相当の出力は「jals の設定オプションを適切な既定値に並べる」ことでは
**達成できない**。GJF は独自の**レイアウトエンジン**と**構文別の Op 発行**を持ち、その両方を
移植して初めて一致する。よって `jals-fmt` は複数の整形パス（jals 独自 style / GJF 互換 / …）を持つ
のではなく、**GJF のエンジンと IR を中核に据え、他のスタイルはすべてその上の Op 発行の差分として
表現する**（§8）。GJF の `Break` 語彙 (`UNIFIED`/`INDEPENDENT`/`FORCED` + `BreakTag`) は
prettier の group/fill の上位互換であり、Eclipse/IntelliJ の per-construct wrap 方針 enum も
（近似の範囲で）この語彙に写せる。**単一エンジンで押し通せるという判断の根拠がこの語彙の広さである。**

**非スコープ**: 他エンジンの解決アルゴリズム（Eclipse の penalty 最小化探索、Palantir の仮説探索、
IntelliJ の greedy-rewind）の移植、および実ツールへの子プロセス委譲。どちらも「エンジンを複数持つ」
ことに帰着するため採らない（§18）。

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

**エンジンが要求する rule 集合は、互いに並列ではない 5 層のパイプラインに階層化される。**
前段で提案した平坦な 3 族分類 (text-normalization / sequence-reordering / layout) は
**設定オプションの分類**としては使えるが、実行構造としては使えない。次の**層 (Layer)** に
再キャストする必要がある。層内は並列だが、**層と層は並列ではなくパイプライン**。

| 層 | 内容 | 層内の並列性 | 前段の構造化に載るか |
|---|---|---|---|
| **L0 前処理トークンパス** | import 整列 / 未使用 import 削除 / modifier 整列 | ○ 並列 | sequence-reordering 族 + 全木解析 1 個 |
| **L1 レイアウトエンジン** | `computeBreaks`（GJF 固有アルゴリズム）+ IR 語彙 | — (単一の基盤) | **rule ではない**。汎用 prettier printer を**置換**する |
| **L2 構文別 Op 発行** | 約 50 の visitor（構文ごとの折り返し） | ○ 並列 | layout 族。**rule が入る主戦場**（§8 の seam S2/S3） |
| **L3 コメント / Javadoc** | コメント付着 + Javadoc 再整形 | △ 付着は枠組み、Javadoc は独立 | 付着=枠組み。**JavadocFormatter は入れ子の別フォーマッタ**で並列でない |
| **L4 後処理パス** | StringWrapper + 最終化 | — | **StringWrapper は出力を再パースする第 2 パス**で並列でない |

- **並列に載る大多数 = L2**（rule 数で言えば大半）。ここは CST→Doc lowering として一様・並列に
  表現でき、提案構造化と綺麗に噛み合う。**rule はここに入る**（どの Level を開くか、break の
  `FillMode`、break を token の前後どちらに置くか、空白を出すか）。
- **並列に載らない 4 点**が実装の要:
  1. **L1 エンジン**は自由に選べない。GJF の `computeBreaks` を移植する必要があり、旧 jals の
     prettier 風 `render`/`fits` では GJF と一致しない（§2.2, §6.2）。**そして単一エンジン方針では
     ここは rule で切り替わらない**（§8）。
  2. **JavadocFormatter** (L3) は Javadoc 文法の入れ子ミニフォーマッタで、構文 rule とは並列でない。
  3. **未使用 import 削除** (L0) は全木の名前収集を要する大域解析で、per-node rule ではない
     （ただし**型解決は不要**＝CST だけで完結する。§4.L0）。
  4. **StringWrapper** (L4) は整形済み出力を**再パースして再レイアウトする第 2 パス**。
     不動点検証まで含み、通常の Doc lowering には還元できない。

したがって答えは:「**L2 は提案構造化に完全に並列で載り、rule もそこに入る。だが実装は L2 だけで
閉じず、rule で切り替わらない固定エンジン (L1) と、並列でない 3 つのサブシステム (L3 Javadoc,
L0 未使用 import, L4 StringWrapper) を別ステージとして加える必要がある**」。

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
**wide 文字で GJF と乖離する**。§7 の byte 一致要件。**幅計測は rule で切り替えない**（seam 外。
エンジンの一部）。

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
 └──────────────┬───────────────────────────┘    │  L1 エンジン: doc.computeBreaks(max-width)
                ▼                                  │      + write(Tok/コメント描画, L3)
 parse → rowan CST ── L2 visitor ──► Doc 木 ───────┘
                     (構文別 Op 発行)        ▲
                                             │ L3: コメント付着(枠組み) / JavadocFormatter(入れ子)
```

モジュール構成案（前段の core/rules 分離を踏襲）。**engine を抽象する trait は置かない**（§14）:

- `ir` — GJF 忠実な `Doc`/`Break`/`Level`/`FillMode`/`BreakTag`/`Indent`（§6.1）。
- `ops` — `OpsBuilder` 相当（`open/close/space/breakOp/breakToFill/forcedBreak/token/blankLineWanted`）。
- `engine` — `computeBreaks` 移植（§6.2）と `write`。
- `visit` — L2 構文別 lowering（rowan CST → Ops）。
- `comments` — L3 付着枠組み（GJF の行ベース規則）。
- `javadoc` — L3 JavadocFormatter（入れ子ミニフォーマッタ）。
- `passes` — L0 (`import_order`, `unused_imports`, `modifier_order`) と L4 (`string_wrapper`,
  `finalize`)。
- `style` — `jals_config::fmt::Config` を**エンジンが直接読む形へ Reify** した固定パラメータ束
  （倍率・列上限・per-construct の `WrapPolicy`/`BraceStyle`/`Spacing` を解決済みの値として持つ）。
  visitor はここだけを見て発行を変える。**これが §8 の seam の実体**。
- `import` — native config → `jals_config::fmt::Config`（**実装済み**。§15、`MAPPING.md`）。

---

## 4. 必要な rule / behavior の完全リスト

各項目に **[族/層]**、**GJF 出典**、**構造化への適合**を付す。適合列:
`◎`=L2 の per-node lowering に並列で載る / `○`=sequence-reordering 族 / `△`=枠組みだが独立機構 /
`✗`=並列でない別ステージ。

本節は**エンジンのネイティブ挙動（= `gjf` プロファイル）**の一覧である。ここに GJF の固定値として
現れる挙動のうち、`jals_config::fmt::Config` に対応 rule があるものは**その rule の既定値**になり、
rule を動かせば挙動も動く（どこが動くかの一覧は §8、rule の選定基準は `MAPPING.md` §2）。

### L0 前処理トークンパス

| # | rule | 出典 | 適合 | 備考 |
|---|---|---|---|---|
| R0.1 | **import 整列**: static→module→normal(ordinal)、各群内 ASCII 昇順、群間に空行 1、重複除去、決して折り返さない | `ImportOrderer.java`, GJSG §3.3.3 | ○ | **static が先**（IntelliJ 既定と逆）。plan/emit 分離、元ノード再利用で多重集合保存 |
| R0.2 | **未使用 import 削除**: 全 `IDENT` 単純名 + Javadoc `@link/@see/@throws` 参照名を収集し、単純名が未出現の import を削除 | `RemoveUnusedImports.java`, README | ✗(大域) | **型解決不要**の名前ヒューリスティック＝CST のみで再現可。per-node でない全木パス。shadowing は GJF も取りこぼす（それも再現） |
| R0.3 | **modifier 整列**: `public protected private abstract default static sealed non-sealed final transient volatile synchronized native strictfp`（JLS/`javax...Modifier` ordinal） | `ModifierOrderer.java`, GJSG §4.8.6 | ○ | 純構文。annotation は先頭へ保持、各コメントは modifier に随伴 |

### L1 レイアウトエンジン（アルゴリズムは rule ではない・固定基盤）

| # | 項目 | 出典 | 適合 |
|---|---|---|---|
| E1 | `computeBreaks`/`computeBroken`/`computeBreakAndSplit`/`getWidth` の意味論（§2.2） | `Doc.java` | 汎用 printer を置換。**rule で切り替わらない** |
| E2 | 列上限 100、block indent 2×mult、継続 4×mult | `Formatter/Indent/JavaFormatterOptions` | GJF では固定、jals では **seam S1**（`[layout]`） |
| E3 | 幅 = UTF-16 code unit（§2.6） | `Doc.computeWidth` | display-width 不可。**rule で切り替わらない** |
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

> 補足: GJF が**やらない**構造変更は、`gjf` プロファイルでは jals もやらない（braces 挿入、
> `int a,b;` 分割、overload 並べ替え、`default`/網羅性追加、wildcard 展開、long suffix `L` の
> 大文字化）。これらは GJSG にはあるが GJF 非実装であり、byte 一致（§18 の T1）のため既定では
> **実装してはならない**。例外は 2 つだけで、どちらも**非既定値でのみ**有効になる:
> `[literals]`（リテラル書換え。既定は全て `preserve`、どの importer からも動かない —
> `MAPPING.md` §5.6）と `[braces] force-*`（IntelliJ `*_BRACE_FORCE` 由来。既定 `never`）。
> **有意トークンを増やすのは後者だけ**である（§9, §20）。

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

### `gjf` プロファイルでの rule 既定値

`gjf` プロファイルは「エンジンのネイティブ挙動」を選ぶ rule 束であって、別のコードパスではない。
GJF はリテラルを一切書き換えないため `[literals]` は全て `preserve`（`MAPPING.md` §5.6 のとおり、
どの importer からも動かない唯一の族）。`[spacing]` / `[braces]` / `[wrapping]` / `[blank-lines]` /
`[comments]` は本節 L2/L3 で列挙した GJF 固定値を既定値として持つ。**`gjf` プロファイルにおいて
「rule を持つこと」自体は出力に影響しない** — 既定値がすべて GJF の固定挙動だからである。

---

## 5. 並列表現可能性の判定（詳細）

前段の 3 族分類を 5 層へ**写像**すると:

- **layout 族 → L2**（◎, ~50 rule, 並列）: 提案どおり per-node lowering に一様に載る。**最大の塊が
  綺麗に並列化できる**。各 lowering は「GJF Op 発行を既定とし、`[wrapping]`/`[braces]`/`[spacing]`
  の解決済み値で発行を分岐する」形になる（§8 seam S2/S3）。
- **sequence-reordering 族 → L0 の import/modifier**（○, 並列）: plan/emit 分離＋元ノード再利用の
  規律がそのまま活きる。多重集合保存。
- **text-normalization 族 → `[literals]`**: GJF はリテラルを変えないので既定はすべて `preserve`。
  非既定値は jals-native プロファイル専用（`MAPPING.md` §4.3）。

**3 族に載らない=並列でない 4 要素**（§1 再掲、これが本質的な追加コスト）:

1. **L1 エンジン**: rule ではなく基盤。汎用 prettier printer を GJF `computeBreaks` で**置換**する。
   これは「並列に足す rule」ではなく「土台の入れ替え」。
2. **未使用 import 削除 (L0/R0.2)**: per-node でなく**全木 1 パスの名前解析**。型解決は不要なので
   `jals-hir` は要らず jals-fmt 内で完結するが、族としては新カテゴリ「全木構文解析パス」。
3. **JavadocFormatter (L3/R3.2)**: Javadoc 文法の**入れ子フォーマッタ**。構文 rule と直交する別機構。
4. **StringWrapper (L4/R4.1)**: 整形出力を再パースする**第 2 パス**＋不動点検証。パイプライン後段。

**総括**: 「すべての rule が並列に表現できるか」への答えは **「L2 は完全に並列。ただし実装は
L2 だけでは閉じず、rule で切り替わらない固定エンジン 1 と、並列でないサブシステム 3 を段として
加える必要がある。3 族分類は 5 層パイプラインへ再構成する」**。

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

## 7. `gjf` プロファイルで byte 一致を達成する落とし穴（実測で潰す）

1. **幅計測**: UTF-16 単位（§2.6）。旧 `UnicodeWidthStr` は wide 文字で乖離。
2. **エンジン差**: prettier 風 `fits` を絶対に流用しない（§2.2）。境界前方走査と mixed-fill が違う。
3. **CST↔javac AST 写像**: メソッドチェーン・二項式のネスト・注釈の型使用/宣言判定など、木形状の
   違いで Op 構造がずれると折り位置が変わる。**GJF 実測出力を golden にした差分テスト**が必須。
4. **配列初期化子の pack 方式**・**arg/param の折り単位**は GJSG 非仕様＝GJF 実挙動を golden で固定。
5. **StringWrapper の不動点**: GJF は wrap 後に AST 不変を検証。jals も同検証を入れないと発散しうる。
6. **AOSP 倍率**: indent/継続のみ差、列上限は 100 のまま。
7. **改行/末尾**: LF・末尾改行 1・連続空行 1（GJSG 非明記の observed 挙動）。

### 7.1 「一致」の定義 — version pin と golden harness（必須の基盤）

**byte 一致は GJF のバージョンを固定して初めて定義できる。** GJF の出力は**リリース間で意図的に
非安定**（だから Spotless 等は GJF バージョンを pin する）。したがって:

1. **`gjf` プロファイルの参照バージョンを固定**する。「byte 一致」= 「GJF `vX.Y.Z` と byte 一致」で
   あり、それより緩い定義は無い。バージョンを上げるのは明示的な意思決定とする。**この厳格さが要求
   されるのは `gjf`/`gjf-aosp` プロファイル（§18 の T1）だけ**で、他ターゲットは近似指標で測る。
2. **golden corpus が目標の唯一の操作的定義**。本書の各「GJF 実測要」項目（配列 pack、arg/param 折り
   単位、幅計測、import 順、text block 内部 indent、StringWrapper 挙動…）は、すべてこの 1 つの
   harness で定義かつ検証される: 実 GJF を Java コーパス（JDK / 主要 OSS）に通した出力を golden とし、
   `jals-fmt` 出力と diff する。不一致は最小再現へ縮約して visitor/engine を修正。
3. **これは純 Rust ワークスペースに JVM + GJF を CI 依存として持ち込む**インフラ判断（現 `CLAUDE.md`
   に JVM 依存は無い）。実行方法は要検討: (a) CI で GJF を実行し golden を生成、(b) golden を生成物
   としてリポジトリに commit（JVM を通常ビルドから隔離、`xtask`/fixture 生成のみ JVM 使用）、
   のいずれか。`jals-tests` の host-only harness に閉じ込めるのが妥当。

### 7.2 不変条件テスト
`jals-fmt/src/invariants.rs`（`#[cfg(test)]`）。golden diff と併走させ、**全プロファイルで**走らせる
（不変条件はプロファイルに依存しない）。5 つの property:

1. 冪等 `fmt(fmt(x))==fmt(x)`。
2. **fail-safe が発火しない**。他の 4 つは *preservation* property で、全体 fallback（`出力==入力`）は
   その全部を同時に満たしてしまう。これが *progress* property で、これが無かったために「ファイルが
   丸ごと未整形で返る」欠陥がテストの網をすり抜けた。
3. §20 のどの行も**到達できない**トークンは個数が完全一致。加えて `RemovesSubtrees` のスコープ内は
   「消えてよいが増えてはならない」（部分多重集合）。**許諾は `License` から読む**（config フィールドから
   再導出すると fail-safe と乖離する — 実際に `force-if` 単独を読んでいた）。到達判定は kind だけでなく
   **row 自身の site 述語**まで見る（kind だけだと無条件行が `COMMA` を claim するので全カンマが視界から
   消える）。共有するのは *scope* までで、**どの lane が答えるか**は共有しない — 比較機構は独立実装のまま
   残し、fail-safe が自分自身に同意するだけの状態を避ける。
4. コメントは 1 つも落ちない。
5. never-panic / 空入力と非空入力の対応。

`src` に置くのは `#[cfg(test)]` のエクスポートが integration test から構造上不可視なため
（`cargo test` は lib を 2 回コンパイルする）。テストのために 5 項目の公開表面を広げるより、
テストを内側に置く方を採った。

---

## 8. rule で何を変えられるか — エンジンの可変点 (seam)

GJF エンジン (`Break` の UNIFIED/INDEPENDENT/FORCED + BreakTag) は prettier group/fill の**上位互換**
であり、Eclipse/IntelliJ の per-construct wrap 方針 enum も（近似の範囲で）この語彙に写せる。
**単一エンジン方針が成立する根拠がこれである。** そのうえで「どこが rule で動き、どこが動かないか」
を先に固定する。ここが曖昧なままだと、rule が増えるたびにエンジンの分岐が増え、結局は複数エンジンに
戻る。

### 8.1 可変点は 4 つだけ (S1–S4)

| seam | 何を変えるか | エンジン内の位置 | 対応する rule 節 |
|---|---|---|---|
| **S1 エンジン定数** | 列上限、indent 幅、継続 indent、tab/space、行終端 | `computeBreaks` の `maxWidth` と `Indent::Const` の焼き込み値 | `[layout]` |
| **S2 発行の形** | どこで `Level` を開くか、その `plus_indent`、break の `FillMode`（`Unified`/`Independent`/`Forced`）、break を token の**前**に置くか**後**に置くか、`Space` を出すか | L2 visitor の分岐 | `[wrapping]`, `[spacing]`, `[braces]` の brace position |
| **S3 強制改行・空行** | `Forced` break の挿入（brace 強制・1 行化の可否）、`blankLineWanted` の本数 | L2 visitor + `OpsBuilder` | `[braces]` の force/keep-on-one-line, `[blank-lines]` |
| **S4 パスの on/off とパラメータ** | L0/L3/L4 の各パスを走らせるか、走らせる時の順序規則 | `passes::Formatter`（パイプライン段）と `passes::token_license`（トークンを変える操作の宣言、§20） | `[imports]`, `[comments]`, `[literals]`, `[wrapping] reflow-long-strings`, `[braces] force-*`, `[layout] formatter-tags` |

**変えないもの（＝「単一エンジン」の定義）**: `computeBreaks`/`computeBroken`/`computeBreakAndSplit`/
`getWidth` の**解決アルゴリズムそのもの**。greedy・単一パス・バックトラックなし・Level 境界で止まる
平坦判定・`mustBreak` の前方伝播・UTF-16 幅計測は、どの rule でも切り替わらない。penalty 最小化探索
も仮説探索も rewind も実装しない。

**唯一の例外に注意 — `braces.force-* = if-multiline`。** これは他のどの rule とも性質が違い、
条件（「文が複数行にまたがるか」）が**エンジン自身の折り結果**で決まり、しかも `{` `}` の挿入が
その判定を生んだ Level の平坦幅を変える。単一パス・バックトラックなしのエンジンに**帰還辺**を
持ち込む唯一の箇所であり、`fmt∘fmt=fmt` が構成的には出てこない（§17）。実装は
「pre-resolution の予測で決め、以後は再判定しない」か「1 回だけ再解決して固定する」かのどちらかに
決め打ちし、**冪等をテストで保証する**こと。他の rule と同じ扱いにしてはならない。

### 8.2 プロファイル = rule 束のプリセット

`gjf` / `gjf-aosp` / `palantir` / `eclipse` / `intellij` / `jals` は**エンジンの切り替えではなく
`Config` の既定値セットの切り替え**である。native config があれば `jals_fmt::import` が同じ `Config`
へ射影する（§15、`MAPPING.md`）ので、プロファイルと importer 出力は**同じ型の値**であり、混ぜて
使える（プロファイルを土台に、native config の非既定値だけを上書きする）。

### 8.3 優先順位（衝突したとき）

rule の一般性とエンジン忠実度が衝突したら、**単一エンジンの一貫性を優先する**。

> **単一エンジンの一貫性 > `gjf` プロファイルの byte 一致 > 他プロファイルの近似精度**

`gjf` 既定で GJF と食い違う点が見つかったら、まず visitor/エンジンのバグを疑って直す。直すために
解決アルゴリズムに GJF 固有の特殊分岐を足す必要があるなら、それは**足さずに §18 の恒久差分表へ
記録する**。この一行があるから「妥協」が場当たりでなく検証可能になる。

### 8.4 rustfmt 風オプションとの関係

前段の考察は rustfmt 風の**設定可能**フォーマッタを志向していた。本書はその語彙を捨て
（`MAPPING.md` §1 の P1–P4）、**Java フォーマッタ 4 者の観測から選んだ rule set**（8 節・176 rule）に
置き換える。単一エンジン方針の下では、rule set はエンジンの上の薄い層であり、**エンジンが表現できない
rule は最初から置かない**（`MAPPING.md` §2 の基準に「到達可能な 2 つのターゲットが食い違う」を課して
いるのはそのため。列揃えのようにエンジンが表現できない概念は rule にせず、native モデル側に型付きで
残す）。

---

## 9. 不変条件の再整理（エンジンのネイティブ挙動の下で）

- **有意トークン保存**: 不変条件は「**§20 の表に宣言された操作を除き有意トークンの多重集合は保存**」。
  例外を散文で数え上げるのはやめ、**`passes::token_license::OPERATIONS` を唯一の定義とする**
  （fail-safe はその表だけを読み、`Config` を見ない）。表の 8 行のうち 7 行は config gate 付きで
  すべて既定 off、8 行目（方言のグループ import 末尾カンマ削除）は**無条件**である。
  なお StringWrapper は「再配置のみ」ではない — 単一リテラルを分割して `+` を**追加する**（§10 参照）。
  保存されるのは各 site が**綴るもの**であり、多重集合ではない。
- **never-panic / lossless**: 未対応ノード・ERROR ノードは **verbatim 出力**へ fallback。最上位に
  fail-safe（出力再パースで新規 syntax error か有意トークン減があれば入力そのまま返す）。
- **冪等**: `computeBreaks` は greedy 純関数、StringWrapper は不動点検証つき。両者とも決定的。
  `fmt∘fmt=fmt` を第一級テストに。
- **コメント完全性**: 付着枠組みは「全コメント Tok を丁度 1 回描画」を debug-assert（biome 由来）。
- **off/on 領域はバイト同一**: `OffOn` は L2 の lowering walk に `Ctx` 経由で効くが、**L4 はそこを
  通らない**。StringWrapper は整形済みテキストを再パースして編集するので、`@formatter:off` を
  破れる最後の段であり、`plan` が自分で `OffOn::scan` を読んで領域に触れる編集を捨てる。
  領域は `sites` に畳まない: `sites` は fail-safe と共有する述語で単一の木の上で答えるが、
  無効領域は run の config の性質で、入力の領域と出力の領域は座標系が違う。ここに置くことで
  **license がパスより広い**側に倒れる — 許諾より少なく変えるパスは fail-safe を踏めない。

---

## 10. Open questions / 要実測

- 配列初期化子の pack（一列/複数列/tabular）と arg/param の厳密な折り単位（GJF 実測）。
- `MAX_LINE_WIDTH` sentinel の厳密値（幅クランプ）。挙動には効かないが移植時に合わせる。
- `//` vs `/* */` の FillMode 割当の細部（trailing block の `breakAndIndentTrailingComment` 条件）。
- `normalize-parameter-comments` 相当を GJF が行うか（`/*name=*/` の空白正規化の有無）。
- text block 内部の再 indent を GJF が行うか（GJSG は「内容は保持」寄り、実測要）。
- ~~StringWrapper が単一リテラルを新トークンへ割ることがあるか~~ → **解決（やる）**。列幅を超える単一
  リテラルは自前の連結へ分割され、`+` が増える。これが `reflow-long-strings` の主目的であり、
  `passes/string_wrapper.rs` の `LITERAL` アームがその実装である。§9 / §20 / `[wrapping]` の doc が
  「多重集合を保存」と書いていたのはこの未解決事項を「やらない」側に賭けていた結果で、いずれも訂正済み。
  結果として §20 の R4.1 行の「変更の種類」は *再配置* ではなく **再分配**（増減あり、site が綴る内容のみ保存）。
- **入力の既存改行を読む rule を再び認めるか（唯一の再検討ポイント）**: `KeepOnOneLine::Preserve` /
  `join-wrapped-lines` / `wrap-long-lines` は「既存の改行を見る」rule で、単一エンジンでは §17 の
  とおり canonical 値へ丸める。丸めが実用上受け入れられないという実測が出た場合に限り、
  「空行の有無」と同じ形（Doc 木を作る**前**に確定する事実として渡す）で再導入を検討する。
  エンジンの解決アルゴリズムに入力空白を持ち込む形では**再導入しない**。

---

# Part II — 単一エンジンで他ターゲットにどこまで寄せるか（Spotless / Eclipse / IntelliJ / Palantir）

Part I は GJF を単一エンジンの基準系として掘り下げた。本 Part は対象を Spotless / Eclipse JDT /
IntelliJ IDEA / Palantir へ拡げ、**「エンジンを増やさずに rule でどこまで寄せられるか」**と
「native config → jalsfmt.toml 自動生成」の実現可能性・問題点・実装を洗う。
**一次情報で確認した結論から述べる。**

## 11. 結論（先出し）

1. **4 者は 4 つの相互非互換なレイアウト解決アルゴリズムを持つ。** これは調査で確定した事実であり、
   単一エンジン + 設定値で**どれか 1 つとしか byte 一致できない**ことを意味する。

   | | 解決アルゴリズム | 内部 IR | 設定規模 | 入力空白依存 |
   |---|---|---|---|---|
   | **GJF** | greedy 単一パス `computeBreaks` | Doc/Break/Level 木 | 不可（固定 + AOSP） | なし（canonical） |
   | **Palantir** | **独自の探索/バックトラック**（GJF の Doc/Level/visitor は継承、break 決定を書換） | GJF 系 IR + 独自 break engine | ほぼ不可（Style 3 種 + formatJavadoc のみ） | なし（canonical） |
   | **Eclipse JDT** | **penalty 最小化探索** `WrapExecutor.findWraps`（memo 化、overflow→penalty 順） | 注釈付きトークン列 + `WrapPolicy` | **~400** | あり（空行保存等） |
   | **IntelliJ** | **greedy + 直近 wrap 候補へ rewind** `WrapProcessor` | `Block` 木 + Wrap/Indent/Align/Spacing | **~270** | **強くあり**（`keep*`） |
   | **Spotless** | —（**オーケストレータ**。上記へ委譲） | 線形 step パイプライン | build DSL | 委譲先次第 |

   **本設計の決定: それでもエンジンは 1 つにする。** 4 つの解決アルゴリズムを移植する道
   （pluggable engine）は取らない。理由は 3 つ:
   - **コスト**: engine あたり多年工数、しかも 4 者すべて version 非安定（結論 6）なので**追随が
     永続コスト**になる。1 つの engine を正しく保つことすら容易ではない。
   - **不変条件**: 入力空白依存 engine（結論 3）を抱えると jals の「有意トークン + 純レイアウト」
     モデルが壊れる（§17, §20）。engine を 1 つに絞れば、この破れを丸ごと回避できる。
   - **効用**: 利用者の実需要は「チームの既存スタイルに揃った出力」であって「byte 一致」ではない
     ことが大半。byte 一致が本当に必要な現場は、その実ツールを CI で走らせればよい（§18）。

   ⇒ **単一エンジン + rule 調整。** byte 一致を狙うのは `gjf`/`gjf-aosp` プロファイルのみ、
   他は**明示的に近似**（精度の階層と恒久差分は §18）。

2. **Spotless は本設計の外側そのもの。** Spotless = `String→String` の step を**宣言順**に連ねる線形
   パイプライン。jals の 5 層パイプライン (L0–L4) は Spotless と**同種の構造物**。⇒ **外側のパイプ
   ライン構造は 4 者すべてに一般化する（○）。内側のレイアウト中核 (L1 engine + IR + L2 emission) は
   engine 固有なので、jals は GJF 系の 1 実装に固定し、他はその上の発行差分で近似する。**

3. **IntelliJ・Eclipse は入力空白に依存する（canonical でない）。** GJF・Part I の設計・jals の
   不変条件はすべて **AST→canonical（空白盲目・冪等 by construction）**。IntelliJ は
   `keepLineBreaks`/`keepBlankLines`/`ij_java_keep_*` により**出力が入力の既存改行の関数**になる。
   意味的に同一で改行だけ違う 2 ファイルが別 byte になる。**単一エンジン方針ではこれを採らない** —
   エンジンが入力空白から読む事実は「有意トークン間に空行があるか」の 1 つだけに限定し、
   行分割の判定は決して入力空白に依存させない（§17）。該当 rule は canonical 値へ丸め、丸めたことを
   診断として報告する。

4. **意味論的操作は pure CST の外。** GJF/Spotless/IntelliJ の未使用 import 削除、IntelliJ の wildcard
   集約 (`class_count_to_use_import_on_demand`) は名前/型解決を要する。GJF の未使用 import 削除は
   名前ヒューリスティックで CST 完結（Part I R0.2）だが、wildcard 集約は「あるパッケージから N 個
   以上 import」の計数＝実 import 解決が要り、pure CST では不可。⇒ **恒久差分**（§18）。

5. **jalsfmt.toml 自動生成は「共通語彙への射影」として行う。** 生成 toml は engine 多重化器ではなく、
   `jals_config::fmt::Config`（8 節・176 rule）**そのもの**であり、単一エンジンが読む唯一の形である。
   射影は**全単射ではない**（写せない native option は importer の native モデル側に型付きで残る）。
   写像の台帳が `MAPPING.md`、実装が `jals_fmt::import`。§15 が生成の流れと限界を扱う。

6. **4 者すべて version 非安定。** 出力はリリース間で変わる。byte 一致を測る `gjf` プロファイルは
   必ず**バージョン pin** 付きで定義する（Part I §7.1）。近似ターゲット（§18 の T2）は
   pin を**指標の注釈**として持つに留める。

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
  ⇒ 単一エンジン方針では **(b)(c) は完全に再現でき（engine 非依存の汎用 step とパス順）、(a) は
  委譲先が GJF なら byte 一致、Eclipse なら近似**という非対称になる。Spotless そのものが
  互換の難所なのではなく、委譲先が難所である。

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
  greedy 一発とは別。⇒ **Style 定数を GJF engine に渡しても Palantir 出力にはならない。** 単一エンジン
  方針ではこの探索エンジンを**移植しない**ので、`palantir` プロファイルは「120 桁 + indent×2 +
  Palantir 寄りの wrap rule 束」による近似であり、method-chain の部分インラインは再現しない
  （§18 の恒久差分）。ただし canonical（空白盲目）なので不変条件は GJF と同じく素直に成立する。

## 13. この構造化は適用できるか — 層別判定

判定列は「単一エンジン方針の下でその層をどう扱うか」。`○`=4 者に一様適用 / `▲`=単一エンジンに固定し
rule で近似（差は §18 に記録）/ `△`=枠は共有、規則は rule。

| 層 | 4 者への一般化 | 判定 |
|---|---|---|
| **外側パイプライン (L0–L4)** | Spotless そのもの。全 engine が pre-pass→engine→comments→post に収まる | ○ 一般化 |
| **L1 レイアウトエンジン** | GJF greedy / Eclipse penalty 探索 / IntelliJ greedy-rewind は**別アルゴリズム** | ▲ **GJF greedy に固定**（seam S1 の定数のみ rule） |
| **IR (Doc-IR)** | GJF=Doc 木 / Eclipse=注釈トークン列 / IntelliJ=Block 木。共有不能 | ▲ **Doc 木に固定**（他 IR は採らない） |
| **L0 import/modifier 整列** | 順序規則は engine 差だが sequence-reordering の枠は共通 | ○ 枠共有・規則は `[imports]` |
| **L2 emission** | engine ごとに break/space/indent の置き方が違う | ▲ 発行を rule で分岐（seam S2/S3）＝**近似の主戦場** |
| **L3 comments** | 付着枠組みは共有可、placement 規則は engine 差 | △ 枠共有・規則は `[comments]` |
| **L4 汎用 step / finalize** | trim / final-newline / off-on / indent 変換は engine 非依存 | ○ 完全共有（= Spotless 汎用 step） |
| **意味論 step（未使用 import / wildcard 集約）** | pure CST 外（GJF 版のみ名前ヒューリスティックで CST 内） | △/✗ 名前ヒューリスティックのみ実装、wildcard 集約は**非対応**（§18） |

**総括**: 「この構造化を適用できるか」への答えは —— **外側の合成パイプライン (L0/L4/step 順)、
sequence-reordering の枠、汎用 step は 4 者に一様適用できる（○）。レイアウトの中核 (L1 engine + IR)
は engine 固有で共有不能なので、そこは GJF 系の 1 実装に固定し、他ターゲットへの追従は L2 emission を
rule で分岐させる近似で行う（▲）。つまり `jals-fmt` は「マルチエンジン・パイプライン」ではなく、
**単一エンジンの周りに Spotless 様の汎用 step 層と rule set を持つフォーマッタ**になる。**

## 14. アーキテクチャ: 単一エンジン + rule set

```
   native config (任意)                     Profile (gjf | gjf-aosp | palantir
   .prefs / XML / .editorconfig                     | eclipse | intellij | jals)
   / spotless DSL(解決済み)                          = Config の既定値プリセット
        │  jals_fmt::import                              │
        └──────────────┬─────────────────────────────────┘
                       ▼
        jals_config::fmt::Config  ── 8 節 / 176 rule（唯一の style 表面）
                       │  style::reify  ← 解決済みパラメータ束（§8 の seam S1–S4）
 ┌─────────────────────┼──────────────────────────────────────────────┐
 │ 単一パイプライン                                                    │
 │  L0 前処理 (import/modifier)  →  L2 発行  →  L1 エンジン  →  L3/L4 │
 │                                   ▲            ▲                    │
 │                            S2/S3 で分岐   S1 の定数のみ             │
 │  汎用 step(trim, final-newline, off-on, indent 変換) / invariant     │
 └──────────────────────────────────────────────────────────────────────┘
```

- **エンジンは 1 つ**: `fn format(cst: &SyntaxNode, style: &Style) -> String`。trait 抽象も
  実装の切り替えも置かない（置いた瞬間にマルチエンジンへ滑る）。`src` は渡さない —
  エンジンが入力から読む唯一の事実「有意トークン間の空行の有無」は L0 で確定して Ops に載せる（§17）。
- **`Profile`**: `Config` の既定値プリセット。**engine の選択子ではない**。native config があれば
  importer 出力で上書きされる（§8.2）。
- **共有 step 層**: 汎用 step（Spotless の generic step と同一物）・off/on 領域・不変条件ハーネス。
- **`jals_fmt::import`（実装済み）**: native config → `Config`（§15、`MAPPING.md`）。native モデルは
  **全数**を型付きで保持し、`Config` へは選別して射影する。写らない option は落とすのではなく
  native モデル側に残る — ただし単一エンジン方針では、それは**将来 engine が読むための保管**ではなく
  **「なぜ写さないか」を型で記録した監査記録**である（§18 の恒久差分表と対応する）。

## 15. jalsfmt.toml 自動生成

**方式: 共通語彙への射影**。生成 toml は `jals_config::fmt::Config` そのもの（8 節・176 rule）であり、
engine 選択子も engine 固有 option の透過テーブルも持たない。生成例（Eclipse `.prefs` 由来）:

```toml
# generated from .settings/org.eclipse.jdt.core.prefs (eclipse 4.31)
# 射影されなかった native option は MAPPING.md §7 / §18 を参照。

[layout]
max-width = 120                      # lineSplit
indent-width = 4                     # tabulation.size

[braces]
type-declaration = "next-line"       # brace_position_for_type_declaration

[wrapping]
call-arguments = "if-long-per-item"  # alignment_for_arguments_in_method_invocation (=48)
```

由来（どのファイルから生成したか）と参照バージョンはコメントとして書き出す。**再生成の追跡に必要
なだけで、エンジンの挙動には影響しない**（engine が 1 つしか無いので、toml に engine 情報を持たせる
意味が無い）。

**検出と生成（jalsfmt.toml 不在時）**: jals-config の発見機構を拡張し優先順で走査（詳細な検出
シグネチャは付録 A.1）:
1. `jalsfmt.toml`（あれば何もしない）
2. `.editorconfig` に `ij_java_*` → IntelliJ importer
3. `.settings/org.eclipse.jdt.core.prefs` / exported formatter XML → Eclipse importer
4. `build.gradle(.kts)`/`pom.xml` の spotless ブロック → 委譲先 importer + 自前 step の rule 化
5. 何も無ければ `gjf` プロファイル

**限界（明示すべき）**:
- **P-gen-1 統一不能**: engine が違えば同一 toml で byte 一致は出せない。射影は共通語彙への
  **不可逆な写像**であり、全単射ではない。写らない option は importer の native モデルに型付きで残り、
  「写らなかった」ことが `MAPPING.md` §7 と §18 に記録される。
- **P-gen-2 空白依存**: IntelliJ は config だけで出力が決まらない（入力空白の関数）。「config→出力」の
  全単射が無く、生成 toml でも入力次第で乖離する。単一エンジンはこの依存を**採らない**ので、
  `KeepOnOneLine::Preserve` / `join-wrapped-lines` / `wrap-long-lines` は canonical 値へ丸め、
  丸めたことを生成時の警告として出す（§17）。
- **P-gen-3 意味論**: 未使用 import 削除・wildcard 集約は toml で表せない挙動。jals-hir か名前ヒュー
  リスティックが要り、toml には有効/無効フラグしか置けない。
- **P-gen-4 Spotless DSL**: build.gradle(Groovy/Kotlin)/pom.xml の spotless ブロックはデータでなく
  コード。完全解析は不可能、よくある形のパターン抽出に留め、未対応は警告。
- **P-gen-5 語彙衝突**: Eclipse brace `next_line_shifted` と IntelliJ `whitesmiths`/`gnu`、wrap token の
  反直感（`split_into_lines`=always）等、native 語彙を機械的に取り違えない写像表が要る。
- **P-gen-6 option 網羅**: Eclipse ~400 / IntelliJ ~270 の**非既定オプションのみ**を射影の入力とし、
  既定値は jals 側の既定（= プロファイル）に委ねる。

## 16. 実装の現実的段階（先に価値が出る順 / 難度昇順）

エンジンは 1 つなので、段階は「エンジンの完成度」と「rule の被覆」の 2 軸で進む。

1. **エンジン中核 + `gjf` プロファイル**（Part I）— IR / `computeBreaks` / L2 visitor / コメント付着。
   rule は既定値のみで動かさない。ここが土台であり、byte diff で検証できる唯一の段。
2. **seam の実装（§8 の S1–S4）** — `style::reify` と visitor の分岐。rule を動かして出力が動くことを
   rule 単位の snapshot テストで固定する。
3. **汎用 step 層 + Spotless 自前 step** — engine 非依存（trim / final-newline / off-on / indent 変換 /
   importOrder / licenseHeader）。委譲先が GJF の Spotless 設定なら、この段で実用的に一致する。
4. **`eclipse` / `intellij` プロファイル + importer 接続** — importer は実装済みなので、
   ここでの作業は **rule → 発行の被覆を埋めること**と、**§18 の恒久差分表を実測で確定すること**。
5. **`palantir` / `jals` プロファイル** — 前者は 120 桁 + Palantir 寄り wrap の近似、後者は
   `[literals]` を含む jals 独自既定。

**「Eclipse engine を移植する」「IntelliJ engine を移植する」という段は無い。** 4 と 5 は
プロファイル（rule 束）の追加であって、エンジンの追加ではない。

検証は段 1 が golden byte diff（Part I §7.1）、段 3–5 が §18 の近似指標。

## 17. 不変条件の再整理（単一エンジン下）

**整形モードは 1 つだけ。** マルチエンジン前提で必要だった canonical / whitespace-retaining の
2 モード分割は、単一エンジン方針では**不要になり、採らない**。

> **エンジンが入力空白から読む事実は 1 つだけ: 2 つの有意トークンの間に空行があるか**
> （本数は `[blank-lines]` の上限へ clamp する）。**行分割の判定は決して入力空白に依存しない。**

この 1 事実は GJF 自身も持つ挙動（メンバ間の空行を保持し、連続空行を 1 へ畳む）であり、jals が新たに
持ち込む不純ではない。しかも L0 で確定して Ops に載る事実なので、L1 の解決アルゴリズムは純粋なまま
であり、**冪等は構成的に保たれる**。

**例外は 1 つだけ: `braces.force-* = if-multiline`**（既定は `never` なので通常は発生しない）。
§8.1 のとおり、この rule だけは条件がエンジン自身の出力を参照するため、冪等が構成的には出てこない。
この rule に限り、冪等は**テストで保証する性質**であり、`fmt∘fmt=fmt` の property test に
`if-multiline` を有効にしたケースを必ず含める。

帰結として:

- **Part I §9 の不変条件がそのまま全プロファイルで成立する。** 冪等 `fmt∘fmt=fmt`、有意トークン
  多重集合保存（§20 の表に宣言された操作を除く）、never-panic、verbatim fallback、off/on 領域、
  コメント完全性。
- **入力の既存改行を読む rule は採らない。** 該当 rule は `Config` に存在する（native モデルからの
  射影先として必要）が、エンジンは**同族で最も意図に近い canonical 値へ丸める**。丸めは `Warning`
  として報告し、黙って無視しない。

  | rule | 由来 | 丸め先 |
  |---|---|---|
  | `braces.keep-*-on-one-line = preserve` | Eclipse `one_line_preserve` / IntelliJ `KEEP_SIMPLE_*_IN_ONE_LINE=true` | `if-single-item`（「1 行で書かれていたものは 1 行のまま」の構造的近似。`never` へ丸めるとその意図を持つ入力すべてが必ず食い違う） |
  | `wrapping.paren-* = preserve` | Eclipse `preserve_positions` | `common-lines` |
  | `wrapping.join-wrapped-lines = false` | Eclipse `join_wrapped_lines=false` / IntelliJ `KEEP_LINE_BREAKS=true` | 常に join（`true`） |
  | `wrapping.wrap-long-lines = true` | IntelliJ `WRAP_LONG_LINES=true` | `false`（既定）。break 点の無い行は折らない — Doc エンジンは発行された break しか取れない |
  | `comments.preserve-line-breaks = true` | IntelliJ `JD_PRESERVE_LINE_FEEDS` | 常に refill（`false`） |

  再検討の条件は §10 に 1 行だけ置いた。
- **golden 検証**: `gjf` プロファイルは pin した実 GJF に対する byte diff（pass/fail）。
  他プロファイルは §18 の近似指標（回帰検出）。全プロファイルで `fmt∘fmt=fmt` を property test に。

---

## 18. 精度の階層と恒久差分（妥協を検証可能にする）

「rule で近似する」は、**どこまで寄せる約束をし、何を諦めたかを列挙して初めて仕様になる**。
本節がその列挙であり、`jals-fmt` が対外的に約束する精度の定義でもある。

### 18.1 精度の階層 (accuracy tier)

| tier | ターゲット | 約束 | 指標 |
|---|---|---|---|
| **T1** | `gjf` / `gjf-aosp` | pin した実 GJF と **byte 一致** | golden byte diff の **pass/fail**（Part I §7.1） |
| **T2** | `eclipse` / `intellij` / `palantir` | **layout 近似**。byte 一致は約束しない | **行分割位置の一致率**＋**差分クラスの列挙**。悪化を回帰として検出 |
| **T3** | `jals` | 参照実装なし | 冪等・不変条件・snapshot のみ |

T1 が byte 一致を狙えるのは「エンジンのネイティブ意味論そのもの」だからであり、努力の量の違いでは
ない。T2 の指標を「一致率」にするのは、byte 一致率だと空白 1 個の差でファイル全体が不一致に落ちて
**改善が見えなくなる**ため。行分割位置（どのトークンの前後で改行したか）の一致率なら、rule の被覆を
増やした効果が単調に効く。

**衝突時の優先順位は §8.3**: 単一エンジンの一貫性 > T1 の byte 一致 > T2 の近似精度。
T1 の差分を消すためにエンジンへ特殊分岐を足すことはせず、18.2 へ記録する。

### 18.2 恒久差分 — rule では埋まらないと確定しているもの

移植しないと決めたアルゴリズムの帰結。**これらは bug ではなく仕様**であり、実測で新たに見つかった
差分もここへ追記していく（`MAPPING.md` §7 の「写像しない native option」と対応する）。

| # | 差分 | 由来 | 影響ターゲット |
|---|---|---|---|
| D1 | **列揃え（column alignment）** — 前の行のトークン列位置に合わせて桁を揃える整形 | Eclipse `align_type_members_on_columns` / `align_variable_declarations_on_columns` / `align_assignment_statements_on_columns` / `alignment_for_*` の `M_INDENT_ON_COLUMN` ビット、IntelliJ `ALIGN_MULTILINE_*` 18 + `ALIGN_CONSECUTIVE_*` | eclipse, intellij |
| D2 | **penalty 最小化の探索結果** — overflow を先に最小化し、次に wrap penalty、tie は totalPenalty で決める大域選択 | Eclipse `WrapExecutor.findWraps` | eclipse |
| D3 | **method-chain の部分インライン** — 過長 Level の prefix だけ同一行に残す | Palantir `PartialInlineability` / `BreakBehaviour` / `Obs` の仮説探索 | palantir |
| D4 | **rewind による再折り** — 行が溢れた時点で直近の wrap 候補（`CHOP_DOWN_IF_LONG` なら list 先頭）まで戻って折り直す | IntelliJ `WrapProcessor` | intellij |
| D5 | **入力の既存改行の保持** — `keep*` 系による「元が 1 行なら 1 行のまま」 | IntelliJ `keepLineBreaks` / `ij_java_keep_*`、Eclipse `join_wrapped_lines=false` | intellij, eclipse |
| D6 | **wildcard 集約 / classpath 依存の import 操作** | IntelliJ `CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND` ほか（§11 結論 4） | intellij |
| D7 | **Javadoc / コメント整形の細部** — `comment.javadoc_paragraphs_tags_with_content`, `comment.new_lines_at_javadoc_boundaries` ほか | Eclipse `CommentsPreparator` / `CommentWrapExecutor` | eclipse |
| D8 | **非対称な paren 位置** — lparen だけ / rparen だけ次行 | IntelliJ `*_LPAREN_ON_NEXT_LINE` と `*_RPAREN_ON_NEXT_LINE` の非対称組合せ（`MAPPING.md` §5.4） | intellij |
| D10 | **import 削除が残す空行** — `import` を 1 つも残さず削除した block の跡に、GJF は空行が 2 本残る（block の前後の空行が隣接するため）。jals は 1 本に正規化する | GJF は `RemoveUnusedImports` / `ImportOrderer` / `StringWrapper` を **レイアウトの後**にテキストパスとして走らせる（`FormatFileCallable.call`）。GJF 自身もこの出力は冪等でなく、2 回目で 1 本に潰れる | gjf |
| D9 | **`spacing.after-case-colon` が到達不能** — colon 形 `case` ラベルの `:` の後でエンジンは常に折るため、`:` と同じ行に続くものが存在しない | 単一エンジンの colon 形 switch レイアウト | eclipse |

D1–D4 は**解決アルゴリズムの違い**なので rule では埋まらない。D5 は §17 の方針で意図的に採らない。
D6 は pure CST の外。D7 は Javadoc 整形器の忠実度の問題で、rule を足せば縮むが完全一致はしない。
D8 は共通語彙の粒度の問題（native モデルには両 bool が残る）。D10 は**パス順序の違い**で、再現するには「整形は冪等」（`CLAUDE.md` / §16）を捨てるしかないので採らない。D9 は rule 側ではなくエンジンの
レイアウトが原因で、`jals-fmt/tests/coverage.rs` の `UNREACHABLE` に理由つきで列挙されている
（そこに載る rule だけが「出力が動かなくてよい」唯一の例外である）。

### 18.3 byte 一致が本当に必要な場合

その実ツールを走らせる以外に方法は無い（Spotless が互換性を委譲で達成しているのはこの理由）。
`jals-fmt` はエンジンを 1 つしか持たないので**子プロセス委譲もクレートの機能としては持たない** —
CI で実ツールを走らせるのは利用者側の選択であり、本設計の対象外である。

**エントリポイント**: 現行公開 API は `FormatOutput::format_source(src, &Config)`（`jals-cli`/
`jals-lsp`/`jals-playground` が依存）。**単一エンジン方針ではこのシグネチャを変えなくてよい** —
プロファイルは `Config` の既定値プリセットにすぎず、`Config` 自体が唯一の style 表面だからである
（プロファイル選択を toml に書けるようにする場合も、`Config` にキー 1 つを足すだけで済む）。

## 19. クレート境界と no_std（ワークスペース規約との整合）

`jals-fmt` は**ポータブルな `no_std` ドメインクレート**（CLAUDE.md:「do not add host filesystem
APIs」）。各部品を規約に沿って配置する:

- **`jals-fmt`（portable, no_std+alloc, wasm）**: 単一エンジン（IR / `computeBreaks` / L2 visitor）、
  汎用 step、invariant、`import`。**host FS/JVM に触れない。** エンジンが 1 つなので、
  **エンジンを抽象する trait も feature も置かない**（wasm ビルドとネイティブビルドで同じコードが
  同じ出力を出す — playground と CLI の一致がタダで手に入る）。
- **config 発見（どのファイルが在るか）**: host I/O ゆえ **`jals-cli`/`jals-lsp`** が担い、
  `jals-storage` の `ProjectView` 経由でバイト列を得る（生 `std::path`/`std::fs` は native adapter のみ）。
- **`import`（バイト列→`Config`）**: `.prefs` / `.editorconfig` の読み手は純 `&str` パーサで
  **portable（no_std / wasm）**。XML の 2 経路は `quick-xml` 依存ゆえ本クレートの **`std` feature**
  裏（既定の wasm ビルドには入らない）。Groovy/Kotlin の spotless ブロック解析は現実的に host 専用で、
  `SpotlessConfig` は**解決済みパイプライン**をモデル化する（`MAPPING.md` §8 の P-gen-4）。
- **golden harness（実ツール × コーパス）**: JVM 依存。**`jals-tests` の host-only harness**に隔離
  （Part I §7.1）。通常ビルドに JVM を持ち込まない。T1 は byte diff、T2 は §18.1 の一致率を記録する。

## 20. CLAUDE.md 不変条件の改訂は依然として利用者判断

CLAUDE.md は**ハード不変条件**として明記している:
> *Formatting is idempotent and preserves the significant token sequence unless an explicitly
> configured text-normalization rule applies.*

単一エンジン方針は、この不変条件との衝突を**半分だけ**解消する。

- **解消した半分（空白依存）**: whitespace-retaining モードを採らないので、冪等は**無条件に**成立し、
  レイアウトは入力空白の関数にならない（§17）。
- **残る半分（トークン列）**: それでも次の操作は**有意トークン列を変える**。しかも
  **どれも text-normalization ではない**。

### 20.1 表（`passes::token_license::OPERATIONS` の実体）

**この表はコードである。** `jals-fmt/src/passes/token_license.rs` の `OPERATIONS` が定義で、
本節はその読み物。fail-safe（`TokenBudget`）は `Config` を一切見ず、この表から導出した `License`
だけを読む。**トークン変更パスを追加するとは、この表に行を足すことである。**

行の順序は**狭いスコープ優先**で、これは load-bearing。グループ import の末尾カンマは `IMPORT_DECL`
の内側にあるので、広い行（未使用 import 削除）が先に来ると狭い行が一度も参照されず、
「import 宣言から何が消えてもよい」という許諾がそのカンマを飲み込む。

ただし `Effect::specificity` が与える順位は**variant 単位**なので、同順位の行どうしの前後は規定
されない。`License::lane` は first-match-wins のままなので、同順位の 2 行が同じトークンに届くと
「表に先に書いた方が勝つ」という同じ masking が 1 段下で起きる。したがって**同順位の行は名指しする
kind が互いに素**でなければならず（site の重なりは表からは決定できない）、node kind でスコープする行
（`RemovesSubtrees`）はその順位に 1 行しか置けない。`equal_specificity_rows_cannot_mask_each_other`
がこれを保証する。

  | # | 操作 | 変更の種類 | gate |
  |---|---|---|---|
  | 1 | **方言 グループ import 末尾カンマ削除** | 削除（`IMPORT_GROUP` 内の `COMMA` 1 個） | **なし（無条件）** |
  | 2 | 長文字列再折り (R4.1) | **再分配**（`+`/文字列片は増減する。site が綴る内容のみ保存） | `[wrapping] reflow-long-strings` |
  | 3 | 未使用 import 削除 (R0.2) | 部分木削除（`IMPORT_DECL`） | `[imports] remove-unused` |
  | 4 | text block 再インデント (R4.1) | 綴り変更（個数は保存） | `[wrapping] reflow-long-strings` |
  | 5 | `[literals]` 数値書換え | 綴り変更（`INT_LITERAL`/`FLOAT_LITERAL` のみ） | `[literals]` |
  | 6 | `[braces] force-*` | **挿入**（`{` `}`） | `[braces] force-*` ×4 |
  | 7 | import 整列 (R0.1) | 並べ替え（多重集合保存 = 免除不要） | `[imports] order` |
  | 8 | modifier 整列 (R0.3) | 並べ替え（多重集合保存 = 免除不要） | `[imports] reorder-modifiers` |

  R0.1 / R0.2 / R0.3 / R4.1 は **`gjf` プロファイルでは既定 on** であり、GJF のネイティブ挙動そのもの。
  2 と 4 が同じ gate で別行なのは、text block は**個数が保存され綴りだけが変わる**別種の効果だから。
  畳んでいた間、消えた text block は多重集合から抜けていて内容検査しか番人が無かった。

  **1 行目が表の存在理由である。** config キーを持たないので、config フィールドから例外を再構成する
  検査には読むものが無く、`[imports] remove-unused` が偶然 on の時だけ（その許諾に相乗りして）
  通っていた。既定 config では拒否され、**ファイル全体が黙って未整形で返っていた**。

**許諾が効果より広い箇所は 3 つあり、いずれも `Effect` の doc で宣言済み**（継承ではなく選択として
書く、が方針）。狭められないのは仕組みではなく**証拠**の問題である:

| 行 | 許諾の幅 | 狭めるのに必要なもの |
|---|---|---|
| 3 未使用 import 削除 | `IMPORT_DECL` 内の**任意の**トークンが欠けてよい（使用中 import の型名でも） | `UnusedImports::used_names` を `lane` に通す per-tree payload。ただし締めると今受理している出力が黙って fallback に変わり得る一方、それを見せる golden corpora が未初期化 submodule |
| 2 長文字列再折り | site は `overflows` を通さないので、実質「ファイル中の全文字列リテラル」 | 過長 site だけに絞る = 入力と出力で site 集合が変わるので比較不能（`Site::Reflow` の doc） |
| 6 `[braces] force-*` | site が無いので `{` `}` の**増加はファイル全域**で無料 | brace 強制の可否を入力木から答える共有述語。`if-multiline` はエンジンの結果を読むので、そもそも入力木では決まらない |

  3 つのうち 6 が最も軽い: 場所違いの brace は多くの場合そもそも parse しないので、fail-safe の
  無条件半分（新規 syntax error なし）が受け止める。**閉じてはいない**（parse が通る挿入は残る）。

### 20.2 採用した改訂

**ワークスペースの中核契約を編集する意思決定**であり、下を**採用済み**（`CLAUDE.md` の Invariants に反映）。

> *Formatting is idempotent. It preserves the significant token multiset except where a declared
> token-changing operation applies. The operations are enumerated as data in
> `jals_fmt::passes::token_license::OPERATIONS` … Seven rows are configured and every one is off (or
> `preserve`) by default … The eighth is unconditional — the jals dialect drops a grouped import's
> trailing comma — so "explicitly configured" is not a complete qualifier, and a new token-changing
> pass belongs in the table, not in prose.*

旧文言（"the four token-changing passes … plus the opt-in literal normalizations and brace forcing"）
からの変更点は 2 つ:

1. **"explicitly configured" を落とした。** 無条件の操作が 1 つあるので、限定子として成立しない。
2. **列挙を散文から表への参照に置き換えた。** 散文の列挙は 4 箇所（§9・§17・§7.2・`lib.rs`）に
   複製されていて、そのすべてが 4 パスと書いたまま実装は 8 操作になっていた。定義を 1 つにすれば
   複製は参照になる。

---

## 付録 A: config ファイル形式リファレンス（`jals_fmt::import` 実装用）

§15 の自動生成/検出を実装可能にするための、各 config ファイルの**具体形式・検出シグネチャ・実例・
パーサの罠**。すべて実ファイル/一次仕様で検証済み。**この付録は単一エンジン方針でも一切変わらない**
— importer は engine ではなく `Config` への射影であり、方針変更の影響を受けないからである。

### A.1 検出の優先順とシグネチャ

| 優先 | ファイル | 検出シグネチャ（内容ベース） | → importer |
|---|---|---|---|
| 1 | `jalsfmt.toml` | 存在 | 何もしない |
| 2 | `.editorconfig`（`ij_` 系キーあり） | `ij_java_*` / `ij_*` キー、または `[*.java]` 節 | intellij |
| 3 | `.idea/codeStyles/Project.xml` | `<component name="ProjectCodeStyleConfiguration">` + `<code_scheme name="Project">` | intellij |
| 4 | exported IDE scheme `*.xml` | ルートが `<code_scheme name="...">`（`<component>` 親なし） | intellij |
| 5 | Eclipse XML profile `*.xml` | `<profile kind="CodeFormatterProfile">` / `<setting id="org.eclipse.jdt.core.formatter.` | eclipse |
| 6 | `.settings/org.eclipse.jdt.core.prefs` | `org.eclipse.jdt.core.formatter.` 行 + `eclipse.preferences.version=` | eclipse |
| 7 | `build.gradle(.kts)` / `pom.xml` | `com.diffplug.spotless` / `spotless {` / `spotless-maven-plugin` | spotless（委譲先の importer を追う） |
| 8 | 上記なし | — | `gjf` プロファイル（既定） |

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
- Spotless 互換 = §12.1 の (a) 委譲先の設定 + (b) 自前 step 再現 + (c) 同一順序。委譲先は上の
  `googleJavaFormat`/`eclipse().configFile`/`palantirJavaFormat` から判定し、**対応するプロファイル
  または importer** を選ぶ（engine は 1 つなので切り替わるのは rule 束だけ）。

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
