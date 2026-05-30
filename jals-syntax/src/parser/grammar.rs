//! Java の文法(再帰下降)。マイルストーン B の vertical slice。
//!
//! 対応範囲: package / import(static・`*`)/ クラス宣言(修飾子・型引数・extends・
//! implements)/ フィールド・メソッド・コンストラクタ / ブロックと文(局所変数(`var`
//! 昇格)・return・if・while・式文)/ 式(リテラル・名前・括弧・単項・二項(優先順位)・
//! 後置 `.`/呼び出し/添字)/ 型(`List<Map<K, V>>`、配列)。`>` 系は隣接判定で合成する。
//!
//! 壊れた入力でも panic せず木を返す。各所に回復集合を置き、`err_and_bump` で前進を保証する。

use super::Parser;
use super::marker::CompletedMarker;
use super::token_set::TokenSet;
use crate::syntax_kind::SyntaxKind::*;

/// クラス本体メンバの開始になりうるトークン(回復に使う)。
const MEMBER_RECOVERY: TokenSet = TokenSet::new(&[
    AT,
    PUBLIC_KW,
    PROTECTED_KW,
    PRIVATE_KW,
    STATIC_KW,
    FINAL_KW,
    ABSTRACT_KW,
    NATIVE_KW,
    SYNCHRONIZED_KW,
    TRANSIENT_KW,
    VOLATILE_KW,
    STRICTFP_KW,
    CLASS_KW,
    INTERFACE_KW,
    ENUM_KW,
    RBRACE,
]);

/// 文の開始になりうるトークン(回復に使う)。
const STMT_RECOVERY: TokenSet = TokenSet::new(&[
    LBRACE, RBRACE, SEMICOLON, IF_KW, WHILE_KW, FOR_KW, RETURN_KW, DO_KW, SWITCH_KW, TRY_KW,
]);

/// プリミティブ型キーワード。
const PRIMITIVE_TYPE: TokenSet = TokenSet::new(&[
    BOOLEAN_KW, BYTE_KW, SHORT_KW, INT_KW, LONG_KW, CHAR_KW, FLOAT_KW, DOUBLE_KW, VOID_KW,
]);

/// 修飾子キーワード(`non-sealed` は別途扱う)。
const MODIFIER_KW: TokenSet = TokenSet::new(&[
    PUBLIC_KW,
    PROTECTED_KW,
    PRIVATE_KW,
    STATIC_KW,
    FINAL_KW,
    ABSTRACT_KW,
    NATIVE_KW,
    SYNCHRONIZED_KW,
    TRANSIENT_KW,
    VOLATILE_KW,
    STRICTFP_KW,
    DEFAULT_KW,
]);

/// リテラルトークン。
const LITERAL_TOKEN: TokenSet = TokenSet::new(&[
    INT_LITERAL,
    FLOAT_LITERAL,
    CHAR_LITERAL,
    STRING_LITERAL,
    TEXT_BLOCK,
    TRUE_KW,
    FALSE_KW,
    NULL_KW,
]);

/// エントリポイント。コンパイル単位をパースする。
pub(super) fn root(p: &mut Parser) {
    let m = p.start();
    // 注: package のアノテーション(package-info)は将来対応。今は素の package のみ。
    if p.at(PACKAGE_KW) {
        package_decl(p);
    }
    while p.at(IMPORT_KW) {
        import_decl(p);
    }
    while !p.at_eof() {
        let before = p.pos();
        type_decl(p);
        // 前進保証(最終防衛線)。
        if p.pos() == before {
            p.err_and_bump("予期しないトークンです");
        }
    }
    m.complete(p, SOURCE_FILE);
}

fn package_decl(p: &mut Parser) {
    let m = p.start();
    p.bump(PACKAGE_KW);
    qualified_name(p, false);
    p.expect(SEMICOLON);
    m.complete(p, PACKAGE_DECL);
}

fn import_decl(p: &mut Parser) {
    let m = p.start();
    p.bump(IMPORT_KW);
    p.eat(STATIC_KW);
    qualified_name(p, true);
    p.expect(SEMICOLON);
    m.complete(p, IMPORT_DECL);
}

/// ドット連結の名前。`allow_star` が真なら末尾 `.*` を許す(import 用)。
fn qualified_name(p: &mut Parser, allow_star: bool) {
    let m = p.start();
    p.expect(IDENT);
    while p.at(DOT) {
        if allow_star && p.nth_at(1, STAR) {
            p.bump(DOT);
            p.bump(STAR);
            break;
        }
        if !p.nth_at(1, IDENT) {
            break;
        }
        p.bump(DOT);
        p.bump(IDENT);
    }
    m.complete(p, QUALIFIED_NAME);
}

/// 型宣言(現状はクラスのみ。それ以外は ERROR で1つ読み飛ばして回復)。
fn type_decl(p: &mut Parser) {
    let m = p.start();
    modifiers(p);
    if p.at(CLASS_KW) {
        class_rest(p, m);
    } else {
        m.abandon(p);
        p.err_and_bump("型宣言を期待しました");
    }
}

/// 修飾子列(アノテーション・修飾子キーワード・`non-sealed`)。常にノードを作る。
fn modifiers(p: &mut Parser) {
    let m = p.start();
    loop {
        if p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
            annotation(p);
        } else if p.at_ts(MODIFIER_KW) {
            p.bump_any();
        } else if at_non_sealed(p) {
            non_sealed(p);
        } else if p.at_contextual_kw("sealed") {
            p.bump_remap(SEALED_KW);
        } else {
            break;
        }
    }
    m.complete(p, MODIFIERS);
}

fn annotation(p: &mut Parser) {
    let m = p.start();
    p.bump(AT);
    qualified_name(p, false);
    if p.at(LPAREN) {
        // 注: アノテーション引数は素通し(中身は後で精密化)。括弧の対応だけ取る。
        arg_list(p);
    }
    m.complete(p, ANNOTATION);
}

/// `non-sealed`(`IDENT("non") MINUS IDENT("sealed")` が隣接)を判定する。
fn at_non_sealed(p: &mut Parser) -> bool {
    p.at_contextual_kw("non")
        && p.nth_at(1, MINUS)
        && p.nth_adjacent(0)
        && p.nth_adjacent(1)
        && p.nth_at(2, IDENT)
        && p.nth_text(2) == "sealed"
}

/// `non-sealed` を1つの `NON_SEALED_KW` ノードに再結合する。
fn non_sealed(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // non
    p.bump_any(); // -
    p.bump_any(); // sealed
    m.complete(p, NON_SEALED_KW);
}

/// `class` 以降(修飾子は呼び出し側が読み済み、`m` はそれを含む開始マーカ)。
fn class_rest(p: &mut Parser, m: super::marker::Marker) {
    p.bump(CLASS_KW);
    p.expect(IDENT);
    if p.at(LT) {
        type_params(p);
    }
    if p.at(EXTENDS_KW) {
        let c = p.start();
        p.bump(EXTENDS_KW);
        type_(p);
        c.complete(p, EXTENDS_CLAUSE);
    }
    if p.at(IMPLEMENTS_KW) {
        let c = p.start();
        p.bump(IMPLEMENTS_KW);
        type_(p);
        while p.eat(COMMA) {
            type_(p);
        }
        c.complete(p, IMPLEMENTS_CLAUSE);
    }
    if p.at_contextual_kw("permits") {
        let c = p.start();
        p.bump_remap(PERMITS_KW);
        type_(p);
        while p.eat(COMMA) {
            type_(p);
        }
        c.complete(p, PERMITS_CLAUSE);
    }
    class_body(p);
    m.complete(p, CLASS_DECL);
}

fn type_params(p: &mut Parser) {
    let m = p.start();
    p.bump(LT);
    while !p.at(GT) && !p.at_eof() {
        let tp = p.start();
        p.expect(IDENT);
        if p.at(EXTENDS_KW) {
            p.bump(EXTENDS_KW);
            type_(p);
            while p.at(AMP) {
                p.bump(AMP);
                type_(p);
            }
        }
        tp.complete(p, TYPE_PARAM);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(GT);
    m.complete(p, TYPE_PARAMS);
}

fn class_body(p: &mut Parser) {
    let m = p.start();
    if !p.expect(LBRACE) {
        m.complete(p, CLASS_BODY);
        return;
    }
    while !p.at(RBRACE) && !p.at_eof() {
        let before = p.pos();
        member(p);
        // 前進保証: member が1トークンも消費しなければ強制的に1つ ERROR で包む。
        // 回復集合の取りこぼしによる無限ループを防ぐ最終防衛線。
        if p.pos() == before {
            p.err_and_bump("予期しないトークンです");
        }
    }
    p.expect(RBRACE);
    m.complete(p, CLASS_BODY);
}

/// クラスメンバ(フィールド/メソッド/コンストラクタ)。
fn member(p: &mut Parser) {
    if p.at(SEMICOLON) {
        // 空メンバ。
        p.bump(SEMICOLON);
        return;
    }
    let m = p.start();
    modifiers(p);

    // コンストラクタ: IDENT '(' で型がない(簡易判定)。
    if p.at(IDENT) && p.nth_at(1, LPAREN) {
        p.bump(IDENT);
        param_list(p);
        if p.at(THROWS_KW) {
            throws_clause(p);
        }
        if p.at(LBRACE) {
            block(p);
        } else {
            p.expect(SEMICOLON);
        }
        m.complete(p, CONSTRUCTOR_DECL);
        return;
    }

    // それ以外は型から始まる(フィールド or メソッド)。
    if !at_type_start(p) {
        m.abandon(p);
        p.err_recover("メンバ宣言を期待しました", MEMBER_RECOVERY);
        return;
    }
    type_(p);
    p.expect(IDENT);
    if p.at(LPAREN) {
        // メソッド。
        param_list(p);
        if p.at(THROWS_KW) {
            throws_clause(p);
        }
        if p.at(LBRACE) {
            block(p);
        } else {
            p.expect(SEMICOLON);
        }
        m.complete(p, METHOD_DECL);
    } else {
        // フィールド(複数宣言子は簡易対応: `= expr` と `,` を読む)。
        if p.eat(EQ) {
            expr(p);
        }
        while p.eat(COMMA) {
            p.expect(IDENT);
            if p.eat(EQ) {
                expr(p);
            }
        }
        p.expect(SEMICOLON);
        m.complete(p, FIELD_DECL);
    }
}

fn throws_clause(p: &mut Parser) {
    let m = p.start();
    p.bump(THROWS_KW);
    type_(p);
    while p.eat(COMMA) {
        type_(p);
    }
    m.complete(p, THROWS_CLAUSE);
}

fn param_list(p: &mut Parser) {
    let m = p.start();
    p.bump(LPAREN);
    while !p.at(RPAREN) && !p.at_eof() {
        let param = p.start();
        modifiers(p);
        type_(p);
        p.eat(ELLIPSIS); // 可変長引数。
        p.expect(IDENT);
        param.complete(p, PARAM);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, PARAM_LIST);
}

// ===== 型 =====

/// 型の開始になりうるか。
fn at_type_start(p: &mut Parser) -> bool {
    p.at_ts(PRIMITIVE_TYPE) || p.at(IDENT) || p.at_contextual_kw("var")
}

fn type_(p: &mut Parser) {
    let m = p.start();
    if p.at_contextual_kw("var") {
        p.bump_remap(VAR_KW);
    } else if p.at_ts(PRIMITIVE_TYPE) {
        p.bump_any();
    } else {
        // 参照型: 名前 + 任意の型引数 + ドット連結。
        p.expect(IDENT);
        if p.at(LT) {
            type_args(p);
        }
        while p.at(DOT) && p.nth_at(1, IDENT) {
            p.bump(DOT);
            p.bump(IDENT);
            if p.at(LT) {
                type_args(p);
            }
        }
    }
    // 配列次元。
    while p.at(LBRACK) && p.nth_at(1, RBRACK) {
        p.bump(LBRACK);
        p.bump(RBRACK);
    }
    m.complete(p, TYPE);
}

fn type_args(p: &mut Parser) {
    let m = p.start();
    p.bump(LT);
    if !p.at(GT) {
        type_arg(p);
        while p.eat(COMMA) {
            type_arg(p);
        }
    }
    // ネストした型引数の閉じは `>` の連続。隣接する `GT` を1つだけ消費する。
    expect_gt(p);
    m.complete(p, TYPE_ARGS);
}

fn type_arg(p: &mut Parser) {
    if p.at(QUESTION) {
        // ワイルドカード `? extends T` / `? super T`。
        p.bump(QUESTION);
        if p.at(EXTENDS_KW) || p.at(SUPER_KW) {
            p.bump_any();
            type_(p);
        }
    } else {
        type_(p);
    }
}

/// 型引数を閉じる `>` を1つ消費する。`>>` などは隣接した複数の `GT` トークンなので、
/// ここでは常に単一の `GT` だけを食べる(残りは外側の `type_args` が食べる)。
fn expect_gt(p: &mut Parser) {
    if !p.eat(GT) {
        p.error("`>` を期待しました");
    }
}

// ===== 文 =====

fn block(p: &mut Parser) {
    let m = p.start();
    p.bump(LBRACE);
    while !p.at(RBRACE) && !p.at_eof() {
        let before = p.pos();
        stmt(p);
        // 前進保証(class_body と同様の最終防衛線)。
        if p.pos() == before {
            p.err_and_bump("予期しないトークンです");
        }
    }
    p.expect(RBRACE);
    m.complete(p, BLOCK);
}

fn stmt(p: &mut Parser) {
    match p.current() {
        LBRACE => block(p),
        SEMICOLON => p.bump(SEMICOLON),
        RETURN_KW => {
            let m = p.start();
            p.bump(RETURN_KW);
            if !p.at(SEMICOLON) {
                expr(p);
            }
            p.expect(SEMICOLON);
            m.complete(p, RETURN_STMT);
        }
        IF_KW => if_stmt(p),
        WHILE_KW => {
            let m = p.start();
            p.bump(WHILE_KW);
            p.expect(LPAREN);
            expr(p);
            p.expect(RPAREN);
            stmt(p);
            m.complete(p, WHILE_STMT);
        }
        _ => {
            // 局所変数宣言か式文。型 + IDENT で始まれば局所変数宣言とみなす。
            if at_local_var_decl(p) {
                local_var_decl(p);
            } else if at_expr_start(p) {
                let m = p.start();
                expr(p);
                p.expect(SEMICOLON);
                m.complete(p, EXPR_STMT);
            } else {
                p.err_recover("文を期待しました", STMT_RECOVERY);
            }
        }
    }
}

fn if_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(IF_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    stmt(p);
    if p.at(ELSE_KW) {
        p.bump(ELSE_KW);
        stmt(p);
    }
    m.complete(p, IF_STMT);
}

/// 局所変数宣言の開始か(`var x` / プリミティブ + IDENT / IDENT…IDENT)。
/// 簡易ヒューリスティック: `var`、またはプリミティブ型の後に IDENT。
fn at_local_var_decl(p: &mut Parser) -> bool {
    if p.at_contextual_kw("var") {
        return true;
    }
    if p.at_ts(PRIMITIVE_TYPE) {
        return true;
    }
    // `IDENT IDENT` または `IDENT< ... >` で宣言とみなす素朴な判定。
    p.at(IDENT) && p.nth_at(1, IDENT)
}

fn local_var_decl(p: &mut Parser) {
    let m = p.start();
    type_(p);
    p.expect(IDENT);
    if p.eat(EQ) {
        expr(p);
    }
    while p.eat(COMMA) {
        p.expect(IDENT);
        if p.eat(EQ) {
            expr(p);
        }
    }
    p.expect(SEMICOLON);
    m.complete(p, LOCAL_VAR_DECL);
}

// ===== 式(優先順位登攀) =====

fn at_expr_start(p: &mut Parser) -> bool {
    p.at_ts(LITERAL_TOKEN)
        || p.at(IDENT)
        || p.at(LPAREN)
        || p.at(THIS_KW)
        || p.at(SUPER_KW)
        || p.at(NEW_KW)
        || p.at(BANG)
        || p.at(TILDE)
        || p.at(PLUS)
        || p.at(MINUS)
        || p.at(PLUS_PLUS)
        || p.at(MINUS_MINUS)
}

/// 式をパースする(エントリ)。
fn expr(p: &mut Parser) {
    expr_bp(p, 0);
}

/// 最小束縛力 `min_bp` で式をパースする(優先順位登攀)。
fn expr_bp(p: &mut Parser, min_bp: u8) -> Option<CompletedMarker> {
    let mut lhs = unary_expr(p)?;

    while let Some((op_bp, op_len, right_assoc)) = peek_bin_op(p) {
        if op_bp < min_bp {
            break;
        }
        let m = lhs.precede(p);
        if p.at(INSTANCEOF_KW) {
            // `instanceof` の右辺は式ではなく型(パターンは将来対応)。
            p.bump(INSTANCEOF_KW);
            type_(p);
        } else {
            // 演算子トークンを消費する(合成 `>>`/`>>>` は複数の GT を読む)。
            consume_bin_op(p, op_len);
            let next_min = if right_assoc { op_bp } else { op_bp + 1 };
            expr_bp(p, next_min);
        }
        lhs = m.complete(p, BINARY_EXPR);
    }
    Some(lhs)
}

/// 次の二項演算子の (束縛力, トークン長, 右結合か) を返す。`>` 系の合成を含む。
fn peek_bin_op(p: &mut Parser) -> Option<(u8, u8, bool)> {
    let bp = match p.current() {
        PIPE_PIPE => return Some((1, 1, false)),
        AMP_AMP => return Some((2, 1, false)),
        PIPE => return Some((3, 1, false)),
        CARET => return Some((4, 1, false)),
        AMP => return Some((5, 1, false)),
        EQ_EQ | BANG_EQ => return Some((6, 1, false)),
        LT | LT_EQ | INSTANCEOF_KW => return Some((7, 1, false)),
        GT => {
            // `>>>` `>>` `>=` `>` の合成。隣接する GT/EQ を数える。
            if p.nth_at(1, GT) && p.nth_adjacent(0) {
                if p.nth_at(2, GT) && p.nth_adjacent(1) {
                    return Some((8, 3, false)); // >>>
                }
                return Some((8, 2, false)); // >>
            }
            if p.nth_at(1, EQ) && p.nth_adjacent(0) {
                return Some((7, 2, false)); // >=
            }
            return Some((7, 1, false)); // >
        }
        LSHIFT => 8,
        PLUS | MINUS => 9,
        STAR | SLASH | PERCENT => 10,
        _ => return None,
    };
    Some((bp, 1, false))
}

/// 二項演算子トークンを `len` 個消費する(合成演算子 `>>`/`>>>`/`>=` のため)。
fn consume_bin_op(p: &mut Parser, len: u8) {
    for _ in 0..len {
        p.bump_any();
    }
}

fn unary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    match p.current() {
        BANG | TILDE | PLUS | MINUS | PLUS_PLUS | MINUS_MINUS => {
            let m = p.start();
            p.bump_any();
            unary_expr(p);
            Some(m.complete(p, UNARY_EXPR))
        }
        _ => postfix_expr(p),
    }
}

fn postfix_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let mut lhs = primary_expr(p)?;
    loop {
        match p.current() {
            DOT if p.nth_at(1, IDENT) => {
                let m = lhs.precede(p);
                p.bump(DOT);
                p.bump(IDENT);
                lhs = m.complete(p, FIELD_ACCESS);
            }
            LPAREN => {
                let m = lhs.precede(p);
                arg_list(p);
                lhs = m.complete(p, CALL_EXPR);
            }
            LBRACK => {
                let m = lhs.precede(p);
                p.bump(LBRACK);
                expr(p);
                p.expect(RBRACK);
                lhs = m.complete(p, INDEX_EXPR);
            }
            PLUS_PLUS | MINUS_MINUS => {
                let m = lhs.precede(p);
                p.bump_any();
                lhs = m.complete(p, POSTFIX_EXPR);
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn primary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let cm = match p.current() {
        _ if p.at_ts(LITERAL_TOKEN) => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LITERAL)
        }
        IDENT => {
            let m = p.start();
            p.bump(IDENT);
            m.complete(p, NAME_REF)
        }
        THIS_KW | SUPER_KW => {
            let m = p.start();
            p.bump_any();
            m.complete(p, NAME_REF)
        }
        LPAREN => {
            let m = p.start();
            p.bump(LPAREN);
            expr(p);
            p.expect(RPAREN);
            m.complete(p, PAREN_EXPR)
        }
        NEW_KW => new_expr(p),
        _ => {
            p.err_and_bump("式を期待しました");
            return None;
        }
    };
    Some(cm)
}

fn new_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(NEW_KW);
    type_(p);
    if p.at(LPAREN) {
        arg_list(p);
    } else if p.at(LBRACK) {
        // 配列生成 `new int[n]`。
        while p.at(LBRACK) {
            p.bump(LBRACK);
            if !p.at(RBRACK) {
                expr(p);
            }
            p.expect(RBRACK);
        }
    }
    m.complete(p, NEW_EXPR)
}

fn arg_list(p: &mut Parser) {
    let m = p.start();
    p.bump(LPAREN);
    while !p.at(RPAREN) && !p.at_eof() {
        expr(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, ARG_LIST);
}
