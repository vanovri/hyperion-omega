use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Join, JoinConstraint,
    JoinOperator, Query, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins,
};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Word};

use crate::error::{LakehouseError, Result};

/// Forbidden SQL functions that could read external host files or induce sleep/DoS.
const FORBIDDEN_FUNCTIONS: &[&str] = &[
    "read_parquet",
    "parquet_scan",
    "read_csv",
    "read_csv_auto",
    "csv_scan",
    "read_json",
    "read_json_auto",
    "read_ndjson",
    "read_text",
    "read_blob",
    "glob",
    "pg_sleep",
    "sleep",
    "current_setting",
    "set_config",
    "version",
];

/// Extracted `ASOF [LEFT] JOIN` specification lowered to `AsofJoinNode` -> `AsofJoinExec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAsofSpec {
    pub left_relation_sql: String,
    pub left_alias: Option<String>,
    pub right_relation_sql: String,
    pub right_alias: Option<String>,
    pub left_key_col: String,
    pub right_key_col: String,
    pub left_ts_col: String,
    pub right_ts_col: String,
    pub strict_lt: bool,
    pub is_left_join: bool,
}

#[derive(Debug, Clone)]
pub struct ValidatedSqlQuery {
    pub normalized_sql: String,
    pub is_explain: bool,
    pub top_level_asof: Option<ExtractedAsofSpec>,
}

/// Token-safe normalizer allowing ergonomic `ASOF [LEFT] JOIN <tbl> [alias] ON <equi> AND <ineq>`
/// in addition to `ASOF JOIN <tbl> MATCH_CONDITION (<ineq>) ON <equi>` without touching string literals.
fn normalize_asof_shorthand(sql: &str) -> Result<(String, bool)> {
    let dialect = DuckDbDialect {};
    let mut tokenizer = Tokenizer::new(&dialect, sql);
    let Ok(tokens) = tokenizer.tokenize() else {
        return Ok((sql.to_string(), false));
    };

    // Detect `ASOF LEFT JOIN` vs `ASOF JOIN` and whether `MATCH_CONDITION` is already present.
    let mut has_asof = false;
    let mut is_asof_left = false;
    let mut has_match_cond = false;

    let significant: Vec<(usize, &Token)> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| !matches!(t, Token::Whitespace(_)))
        .collect();

    for idx in 0..significant.len() {
        if let Token::Word(w) = significant[idx].1 {
            if w.value.eq_ignore_ascii_case("ASOF") {
                has_asof = true;
                if idx + 1 < significant.len() {
                    if let Token::Word(w_next) = significant[idx + 1].1 {
                        if w_next.value.eq_ignore_ascii_case("LEFT") {
                            is_asof_left = true;
                        }
                    }
                }
            } else if w.value.eq_ignore_ascii_case("MATCH_CONDITION") {
                has_match_cond = true;
            }
        }
    }

    if !has_asof {
        return Ok((sql.to_string(), false));
    }

    // Strip `LEFT` (and optional `OUTER`) right after `ASOF` so DuckDbDialect parses `JoinOperator::AsOf`
    let mut filtered_tokens: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if let Token::Word(w) = &tokens[i] {
            if w.value.eq_ignore_ascii_case("ASOF") {
                filtered_tokens.push(tokens[i].clone());
                i += 1;
                // Preserve whitespace, skip LEFT / OUTER before JOIN
                while i < tokens.len() {
                    match &tokens[i] {
                        Token::Whitespace(_) => {
                            filtered_tokens.push(tokens[i].clone());
                            i += 1;
                        }
                        Token::Word(w2)
                            if w2.value.eq_ignore_ascii_case("LEFT")
                                || w2.value.eq_ignore_ascii_case("OUTER") =>
                        {
                            i += 1;
                        }
                        _ => break,
                    }
                }
                continue;
            }
        }
        filtered_tokens.push(tokens[i].clone());
        i += 1;
    }

    let cleaned_sql: String = filtered_tokens.iter().map(ToString::to_string).collect();
    if has_match_cond {
        return Ok((cleaned_sql, is_asof_left));
    }

    // Rewrite `ASOF JOIN <rel> ON <cond1> AND <cond2>` into
    // `ASOF JOIN <rel> MATCH_CONDITION (<ineq>) ON <equi>` via token boundary inspection.
    let mut tok2 = Tokenizer::new(&dialect, &cleaned_sql);
    let Ok(tks) = tok2.tokenize() else {
        return Ok((cleaned_sql, is_asof_left));
    };

    // Find `ASOF` -> `JOIN` -> first top-level `ON`
    let mut out = String::with_capacity(cleaned_sql.len() + 32);
    let mut pos = 0;
    let mut rewritten = false;

    while pos < tks.len() {
        let is_asof_tok = matches!(&tks[pos], Token::Word(Word { value, .. }) if value.eq_ignore_ascii_case("ASOF"));
        if !is_asof_tok || rewritten {
            out.push_str(&tks[pos].to_string());
            pos += 1;
            continue;
        }

        // Emit up to top-level `ON`
        let mut depth = 0_i32;
        let mut on_idx = None;
        let mut scan = pos;
        while scan < tks.len() {
            match &tks[scan] {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::Word(w) if depth == 0 && w.value.eq_ignore_ascii_case("ON") => {
                    on_idx = Some(scan);
                    break;
                }
                _ => {}
            }
            scan += 1;
        }

        let Some(found_on) = on_idx else {
            out.push_str(&tks[pos].to_string());
            pos += 1;
            continue;
        };

        for t in &tks[pos..found_on] {
            out.push_str(&t.to_string());
        }

        // Collect ON predicate tokens until top-level clause terminator (WHERE, GROUP, ORDER, LIMIT, HAVING, WINDOW, QUALIFY, JOIN, INNER, LEFT, RIGHT, CROSS, UNION, ;)
        let mut pred_end = found_on + 1;
        let mut p_depth = 0_i32;
        let mut top_and_idx = None;
        while pred_end < tks.len() {
            match &tks[pred_end] {
                Token::LParen => p_depth += 1,
                Token::RParen => {
                    if p_depth == 0 {
                        break;
                    }
                    p_depth -= 1;
                }
                Token::SemiColon if p_depth == 0 => break,
                Token::Word(w) if p_depth == 0 => {
                    let upper = w.value.to_ascii_uppercase();
                    if matches!(
                        upper.as_str(),
                        "WHERE"
                            | "GROUP"
                            | "ORDER"
                            | "LIMIT"
                            | "HAVING"
                            | "WINDOW"
                            | "QUALIFY"
                            | "UNION"
                            | "INTERSECT"
                            | "EXCEPT"
                            | "JOIN"
                            | "INNER"
                            | "LEFT"
                            | "RIGHT"
                            | "FULL"
                            | "CROSS"
                            | "ASOF"
                    ) {
                        break;
                    }
                    if upper == "AND" && top_and_idx.is_none() {
                        top_and_idx = Some(pred_end);
                    }
                }
                _ => {}
            }
            pred_end += 1;
        }

        if let Some(and_pos) = top_and_idx {
            let left_pred: String = tks[found_on + 1..and_pos]
                .iter()
                .map(ToString::to_string)
                .collect();
            let right_pred: String = tks[and_pos + 1..pred_end]
                .iter()
                .map(ToString::to_string)
                .collect();

            let left_has_ineq = left_pred.contains(">=")
                || left_pred.contains("<=")
                || left_pred.contains('>')
                || left_pred.contains('<');
            let (ineq_part, equi_part) = if left_has_ineq {
                (left_pred.trim(), right_pred.trim())
            } else {
                (right_pred.trim(), left_pred.trim())
            };

            out.push_str(&format!("MATCH_CONDITION ({ineq_part}) ON {equi_part} "));
            rewritten = true;
            pos = pred_end;
        } else {
            // Single predicate without AND -> will fail AST guard cleanly as missing equi or match condition
            out.push_str(&tks[found_on].to_string());
            pos = found_on + 1;
        }
    }

    Ok((out, is_asof_left))
}

/// Validates SQL against the Channel 17 Forensic AST Security Policy:
/// 1. Single statement only (blocks stacked injection `SELECT 1; DROP TABLE ...`)
/// 2. Read-only `SELECT` / non-analyze `EXPLAIN SELECT` only (blocks all DDL/DML/COPY/SET/PRAGMA/CALL)
/// 3. Blocks `WITH RECURSIVE`, `SELECT INTO`, and external file/sleep functions
/// 4. Blocks explosive `CROSS JOIN`, implicit comma joins (`FROM a, b`), unpredicated joins,
///    and tautological join conditions (`ON 1=1`, `ON true`, `ON a.x = a.x`)
pub fn validate_forensic_sql(sql: &str, max_depth: usize) -> Result<ValidatedSqlQuery> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err(LakehouseError::MultiStatementRejected(0));
    }

    let (normalized_sql, is_asof_left) = normalize_asof_shorthand(trimmed)?;
    let dialect = DuckDbDialect {};
    let statements = Parser::parse_sql(&dialect, &normalized_sql)
        .map_err(|e| LakehouseError::SqlParse(e.to_string()))?;

    if statements.len() != 1 {
        return Err(LakehouseError::MultiStatementRejected(statements.len()));
    }

    let stmt = &statements[0];
    match stmt {
        Statement::Query(query) => {
            validate_query(query, 0, max_depth)?;
            let top_level_asof = extract_top_level_asof(query, is_asof_left)?;
            Ok(ValidatedSqlQuery {
                normalized_sql,
                is_explain: false,
                top_level_asof,
            })
        }
        Statement::Explain {
            statement,
            analyze,
            ..
        } => {
            if *analyze {
                return Err(LakehouseError::ReadOnlyViolation(
                    "EXPLAIN ANALYZE is forbidden on forensic endpoint; use EXPLAIN".into(),
                ));
            }
            match statement.as_ref() {
                Statement::Query(q) => {
                    validate_query(q, 0, max_depth)?;
                    let top_level_asof = extract_top_level_asof(q, is_asof_left)?;
                    Ok(ValidatedSqlQuery {
                        normalized_sql,
                        is_explain: true,
                        top_level_asof,
                    })
                }
                other => Err(LakehouseError::ReadOnlyViolation(format!(
                    "EXPLAIN target must be SELECT query, got: {}",
                    statement_kind_name(other)
                ))),
            }
        }
        other => Err(LakehouseError::ReadOnlyViolation(format!(
            "Statement '{}' is strictly forbidden (SELECT read-only enforced)",
            statement_kind_name(other)
        ))),
    }
}

fn statement_kind_name(stmt: &Statement) -> &'static str {
    match stmt {
        Statement::Insert(_) => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Delete(_) => "DELETE",
        Statement::Drop { .. } => "DROP",
        Statement::CreateTable(_) => "CREATE TABLE",
        Statement::CreateView { .. } => "CREATE VIEW",
        Statement::CreateIndex(_) => "CREATE INDEX",
        Statement::AlterTable { .. } => "ALTER TABLE",
        Statement::Truncate { .. } => "TRUNCATE",
        Statement::Copy { .. } => "COPY",
        Statement::SetVariable { .. } => "SET",
        Statement::StartTransaction { .. } => "START TRANSACTION",
        Statement::Commit { .. } => "COMMIT",
        Statement::Rollback { .. } => "ROLLBACK",
        Statement::Grant { .. } => "GRANT",
        Statement::Revoke { .. } => "REVOKE",
        Statement::Call(_) => "CALL",
        Statement::Execute { .. } => "EXECUTE",
        Statement::Prepare { .. } => "PREPARE",
        _ => "NON_SELECT_STATEMENT",
    }
}

fn validate_query(query: &Query, depth: usize, max_depth: usize) -> Result<()> {
    if depth > max_depth {
        return Err(LakehouseError::AstComplexityExceeded(depth));
    }

    if let Some(with) = &query.with {
        if with.recursive {
            return Err(LakehouseError::ForbiddenConstruct(
                "WITH RECURSIVE CTEs are blocked to prevent unbounded recursion".into(),
            ));
        }
        for cte in &with.cte_tables {
            validate_query(&cte.query, depth + 1, max_depth)?;
        }
    }

    validate_set_expr(&query.body, depth + 1, max_depth)?;

    if let Some(limit) = &query.limit {
        validate_expr(limit, depth + 1, max_depth)?;
    }

    Ok(())
}

fn validate_set_expr(body: &SetExpr, depth: usize, max_depth: usize) -> Result<()> {
    if depth > max_depth {
        return Err(LakehouseError::AstComplexityExceeded(depth));
    }
    match body {
        SetExpr::Select(select) => validate_select(select, depth + 1, max_depth),
        SetExpr::Query(q) => validate_query(q, depth + 1, max_depth),
        SetExpr::SetOperation { left, right, .. } => {
            validate_set_expr(left, depth + 1, max_depth)?;
            validate_set_expr(right, depth + 1, max_depth)
        }
        SetExpr::Values(_) => Ok(()),
        other => Err(LakehouseError::ReadOnlyViolation(format!(
            "Forbidden query body variant: {other}"
        ))),
    }
}

fn validate_select(select: &Select, depth: usize, max_depth: usize) -> Result<()> {
    if select.into.is_some() {
        return Err(LakehouseError::ReadOnlyViolation(
            "SELECT ... INTO is forbidden on read-only forensic endpoint".into(),
        ));
    }

    // Block implicit comma cross-joins (`FROM trades, quotes`)
    if select.from.len() > 1 {
        return Err(LakehouseError::ExplosiveCrossJoinRejected(
            "Implicit comma join (FROM a, b) is blocked; use explicit JOIN with an equality ON predicate".into(),
        ));
    }

    for twj in &select.from {
        validate_table_with_joins(twj, depth + 1, max_depth)?;
    }

    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                validate_expr(e, depth + 1, max_depth)?;
            }
            SelectItem::QualifiedWildcard(_, _) | SelectItem::Wildcard(_) => {}
        }
    }

    if let Some(selection) = &select.selection {
        validate_expr(selection, depth + 1, max_depth)?;
    }

    if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for e in exprs {
            validate_expr(e, depth + 1, max_depth)?;
        }
    }

    if let Some(having) = &select.having {
        validate_expr(having, depth + 1, max_depth)?;
    }

    Ok(())
}

fn validate_table_with_joins(twj: &TableWithJoins, depth: usize, max_depth: usize) -> Result<()> {
    validate_table_factor(&twj.relation, depth + 1, max_depth)?;
    for join in &twj.joins {
        validate_join(join, depth + 1, max_depth)?;
    }
    Ok(())
}

fn validate_table_factor(tf: &TableFactor, depth: usize, max_depth: usize) -> Result<()> {
    if depth > max_depth {
        return Err(LakehouseError::AstComplexityExceeded(depth));
    }
    match tf {
        TableFactor::Table { name, args, .. } => {
            let raw_name = name.to_string().to_ascii_lowercase();
            let base = raw_name.rsplit('.').next().unwrap_or(&raw_name);
            if FORBIDDEN_FUNCTIONS.contains(&base) || raw_name.contains('/') || raw_name.contains('\\') || raw_name.ends_with(".parquet") {
                return Err(LakehouseError::ForbiddenConstruct(format!(
                    "External file table or function '{name}' is forbidden"
                )));
            }
            if args.is_some() {
                return Err(LakehouseError::ForbiddenConstruct(format!(
                    "Table-valued function '{name}(...)' is forbidden"
                )));
            }
            Ok(())
        }
        TableFactor::Derived { subquery, .. } => validate_query(subquery, depth + 1, max_depth),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => validate_table_with_joins(table_with_joins, depth + 1, max_depth),
        TableFactor::TableFunction { expr, .. } => {
            Err(LakehouseError::ForbiddenConstruct(format!(
                "Table function '{expr}' is forbidden"
            )))
        }
        other => Err(LakehouseError::ForbiddenConstruct(format!(
            "Unsupported table factor '{other}'"
        ))),
    }
}

fn validate_join(join: &Join, depth: usize, max_depth: usize) -> Result<()> {
    validate_table_factor(&join.relation, depth + 1, max_depth)?;

    match &join.join_operator {
        JoinOperator::CrossJoin | JoinOperator::CrossApply | JoinOperator::OuterApply => {
            Err(LakehouseError::ExplosiveCrossJoinRejected(
                "CROSS JOIN / APPLY is strictly blocked to prevent Cartesian memory explosion".into(),
            ))
        }
        JoinOperator::Join(c)
        | JoinOperator::Inner(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c)
        | JoinOperator::Semi(c)
        | JoinOperator::LeftSemi(c)
        | JoinOperator::RightSemi(c)
        | JoinOperator::Anti(c)
        | JoinOperator::LeftAnti(c)
        | JoinOperator::RightAnti(c) => validate_join_constraint(c, depth + 1, max_depth),
        JoinOperator::AsOf {
            match_condition,
            constraint,
        } => {
            validate_expr(match_condition, depth + 1, max_depth)?;
            if !has_asof_inequality_columns(match_condition) {
                return Err(LakehouseError::TautologicalJoinRejected(
                    "ASOF JOIN MATCH_CONDITION requires a valid timestamp inequality between two distinct columns".into(),
                ));
            }
            validate_join_constraint(constraint, depth + 1, max_depth)
        }
    }
}

fn validate_join_constraint(constraint: &JoinConstraint, depth: usize, max_depth: usize) -> Result<()> {
    match constraint {
        JoinConstraint::None => Err(LakehouseError::ExplosiveCrossJoinRejected(
            "JOIN without ON or USING constraint is blocked".into(),
        )),
        JoinConstraint::Natural => Err(LakehouseError::ExplosiveCrossJoinRejected(
            "NATURAL JOIN is blocked; specify explicit ON equality keys".into(),
        )),
        JoinConstraint::Using(cols) => {
            if cols.is_empty() {
                Err(LakehouseError::ExplosiveCrossJoinRejected(
                    "JOIN USING () cannot be empty".into(),
                ))
            } else {
                Ok(())
            }
        }
        JoinConstraint::On(expr) => {
            validate_expr(expr, depth + 1, max_depth)?;
            if !has_selective_column_equi_join(expr) {
                return Err(LakehouseError::TautologicalJoinRejected(format!(
                    "JOIN ON condition '{expr}' lacks a selective cross-table column equality (tautologies like ON 1=1 or ON a.x=a.x are blocked)"
                )));
            }
            Ok(())
        }
    }
}

/// Ensures a `JOIN ON` expression contains at least one conjunctive equality `lhs_col = rhs_col`
/// where `lhs_col` and `rhs_col` are column references that are NOT syntactically identical!
fn has_selective_column_equi_join(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => has_selective_column_equi_join(inner),
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => {
                // Both sides cannot be tautologies if one is a real column equi-join,
                // and neither side may be an `OR true` bypass
                has_selective_column_equi_join(left) || has_selective_column_equi_join(right)
            }
            BinaryOperator::Eq => {
                let l_col = extract_column_ident_string(left);
                let r_col = extract_column_ident_string(right);
                match (l_col, r_col) {
                    (Some((l_qual, l_name)), Some((r_qual, r_name))) => {
                        if let (Some(lq), Some(rq)) = (&l_qual, &r_qual) {
                            !lq.eq_ignore_ascii_case(rq)
                        } else {
                            // Unqualified: require distinct column names (blocks `x = x`)
                            !l_name.eq_ignore_ascii_case(&r_name)
                        }
                    }
                    _ => false,
                }
            }
            // Note: `OR` at top level (`ON a.id = b.id OR 1 = 1`) is NOT allowed to satisfy equi-join!
            _ => false,
        },
        _ => false,
    }
}

fn has_asof_inequality_columns(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => has_asof_inequality_columns(inner),
        Expr::BinaryOp { left, op, right } => {
            if matches!(
                op,
                BinaryOperator::GtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Lt
            ) {
                match (
                    extract_column_ident_string(left),
                    extract_column_ident_string(right),
                ) {
                    (Some((lq, ln)), Some((rq, rn))) => {
                        if let (Some(lq), Some(rq)) = (lq, rq) {
                            !lq.eq_ignore_ascii_case(&rq)
                        } else {
                            !ln.eq_ignore_ascii_case(&rn)
                        }
                    }
                    _ => false,
                }
            } else {
                false
            }
        }
        _ => false,
    }
}

fn extract_column_ident_string(expr: &Expr) -> Option<(Option<String>, String)> {
    match expr {
        Expr::Nested(inner) => extract_column_ident_string(inner),
        Expr::Identifier(id) => Some((None, id.value.clone())),
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            let col = parts.last()?.value.clone();
            let qual = parts[..parts.len() - 1]
                .iter()
                .map(|p| p.value.as_str())
                .collect::<Vec<_>>()
                .join(".");
            Some((Some(qual), col))
        }
        _ => None,
    }
}

fn validate_expr(expr: &Expr, depth: usize, max_depth: usize) -> Result<()> {
    if depth > max_depth {
        return Err(LakehouseError::AstComplexityExceeded(depth));
    }
    match expr {
        Expr::Function(func) => {
            let fname = func.name.to_string().to_ascii_lowercase();
            let bare = fname.rsplit('.').next().unwrap_or(&fname);
            if FORBIDDEN_FUNCTIONS.contains(&bare) {
                return Err(LakehouseError::ForbiddenConstruct(format!(
                    "Function '{}' is forbidden by security policy",
                    func.name
                )));
            }
            if let FunctionArguments::List(arg_list) = &func.args {
                for arg in &arg_list.args {
                    match arg {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named {
                            arg: FunctionArgExpr::Expr(e),
                            ..
                        }
                        | FunctionArg::ExprNamed {
                            arg: FunctionArgExpr::Expr(e),
                            ..
                        } => validate_expr(e, depth + 1, max_depth)?,
                        _ => {}
                    }
                }
            }
            Ok(())
        }
        Expr::BinaryOp { left, right, .. } => {
            validate_expr(left, depth + 1, max_depth)?;
            validate_expr(right, depth + 1, max_depth)
        }
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Nested(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull(inner)
        | Expr::IsNotNull(inner) => validate_expr(inner, depth + 1, max_depth),
        Expr::Between {
            expr: e,
            low,
            high,
            ..
        } => {
            validate_expr(e, depth + 1, max_depth)?;
            validate_expr(low, depth + 1, max_depth)?;
            validate_expr(high, depth + 1, max_depth)
        }
        Expr::InList { expr: e, list, .. } => {
            validate_expr(e, depth + 1, max_depth)?;
            for item in list {
                validate_expr(item, depth + 1, max_depth)?;
            }
            Ok(())
        }
        Expr::Subquery(sq) => validate_query(sq, depth + 1, max_depth),
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            validate_expr(e, depth + 1, max_depth)?;
            validate_query(subquery, depth + 1, max_depth)
        }
        Expr::Exists { subquery, .. } => validate_query(subquery, depth + 1, max_depth),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(op) = operand {
                validate_expr(op, depth + 1, max_depth)?;
            }
            for cw in conditions {
                validate_expr(&cw.condition, depth + 1, max_depth)?;
                validate_expr(&cw.result, depth + 1, max_depth)?;
            }
            if let Some(el) = else_result {
                validate_expr(el, depth + 1, max_depth)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn table_factor_info(tf: &TableFactor) -> Option<(String, Option<String>)> {
    match tf {
        TableFactor::Table { name, alias, .. } => {
            let tbl = name.to_string();
            let al = alias.as_ref().map(|a| a.name.value.clone());
            Some((tbl, al))
        }
        TableFactor::Derived {
            subquery, alias, ..
        } => {
            let sub = format!("({subquery})");
            let al = alias.as_ref().map(|a| a.name.value.clone());
            Some((sub, al))
        }
        _ => None,
    }
}

fn extract_top_level_asof(query: &Query, is_left_join: bool) -> Result<Option<ExtractedAsofSpec>> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    if select.from.len() != 1 || select.from[0].joins.len() != 1 {
        return Ok(None);
    }
    let twj = &select.from[0];
    let join = &twj.joins[0];
    let JoinOperator::AsOf {
        match_condition,
        constraint,
    } = &join.join_operator
    else {
        return Ok(None);
    };

    let Some((left_relation_sql, left_alias)) = table_factor_info(&twj.relation) else {
        return Ok(None);
    };
    let Some((right_relation_sql, right_alias)) = table_factor_info(&join.relation) else {
        return Ok(None);
    };

    let left_ref_name = left_alias
        .as_deref()
        .unwrap_or(left_relation_sql.as_str());
    let right_ref_name = right_alias
        .as_deref()
        .unwrap_or(right_relation_sql.as_str());

    // Extract equi-join columns from ON
    let (left_key_col, right_key_col) = match constraint {
        JoinConstraint::Using(cols) => {
            let c = cols[0].to_string();
            (c.clone(), c)
        }
        JoinConstraint::On(on_expr) => extract_equi_pair(on_expr, left_ref_name, right_ref_name)
            .ok_or_else(|| {
                LakehouseError::SqlParse("Unable to resolve ASOF JOIN equality key columns".into())
            })?,
        _ => {
            return Err(LakehouseError::ExplosiveCrossJoinRejected(
                "ASOF JOIN requires ON or USING key".into(),
            ))
        }
    };

    // Extract inequality timestamp columns from MATCH_CONDITION
    let (left_ts_col, right_ts_col, strict_lt) =
        extract_asof_ts_pair(match_condition, left_ref_name, right_ref_name).ok_or_else(|| {
            LakehouseError::SqlParse(
                "Unable to resolve ASOF JOIN timestamp inequality columns".into(),
            )
        })?;

    Ok(Some(ExtractedAsofSpec {
        left_relation_sql,
        left_alias,
        right_relation_sql,
        right_alias,
        left_key_col,
        right_key_col,
        left_ts_col,
        right_ts_col,
        strict_lt,
        is_left_join,
    }))
}

fn extract_equi_pair(
    expr: &Expr,
    left_ref: &str,
    right_ref: &str,
) -> Option<(String, String)> {
    match expr {
        Expr::Nested(inner) => extract_equi_pair(inner, left_ref, right_ref),
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => {
                extract_equi_pair(left, left_ref, right_ref)
                    .or_else(|| extract_equi_pair(right, left_ref, right_ref))
            }
            BinaryOperator::Eq => {
                let (lq, ln) = extract_column_ident_string(left)?;
                let (rq, rn) = extract_column_ident_string(right)?;
                if lq.as_deref().is_some_and(|q| q.eq_ignore_ascii_case(right_ref))
                    || rq.as_deref().is_some_and(|q| q.eq_ignore_ascii_case(left_ref))
                {
                    Some((rn, ln))
                } else {
                    Some((ln, rn))
                }
            }
            _ => None,
        },
        _ => None,
    }
}

fn extract_asof_ts_pair(
    expr: &Expr,
    left_ref: &str,
    right_ref: &str,
) -> Option<(String, String, bool)> {
    match expr {
        Expr::Nested(inner) => extract_asof_ts_pair(inner, left_ref, right_ref),
        Expr::BinaryOp { left, op, right } => {
            let (lq, ln) = extract_column_ident_string(left)?;
            let (_rq, rn) = extract_column_ident_string(right)?;
            let left_is_lhs = !lq
                .as_deref()
                .is_some_and(|q| q.eq_ignore_ascii_case(right_ref));
            let _ = left_ref;

            match (op, left_is_lhs) {
                // left.ts >= right.ts -> right.ts <= left.ts (strict_lt = false)
                (BinaryOperator::GtEq, true) => Some((ln, rn, false)),
                // left.ts > right.ts -> right.ts < left.ts (strict_lt = true)
                (BinaryOperator::Gt, true) => Some((ln, rn, true)),
                // right.ts <= left.ts -> (left = rn, right = ln, strict_lt = false)
                (BinaryOperator::LtEq, false) => Some((rn, ln, false)),
                // right.ts < left.ts -> (left = rn, right = ln, strict_lt = true)
                (BinaryOperator::Lt, false) => Some((rn, ln, true)),
                // Fallback if unqualified `t_ts >= q_ts`
                (BinaryOperator::LtEq, true) => Some((rn, ln, false)),
                (BinaryOperator::Lt, true) => Some((rn, ln, true)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Rewrites top-level `FROM <left> ASOF JOIN <right> ...` into `FROM __hyperion_asof_rel`
/// while converting qualified references (`l_alias.col` -> `__l__col` or preserving unique names)
/// — wait: even simpler! In DataFusion, `ViewTable::try_new(asof_plan, None)` exposes columns,
/// OR if we build the outer `LogicalPlan` directly or alias duplicate columns cleanly!
pub fn rewrite_asof_query_to_view_sql(
    normalized_sql: &str,
    spec: &ExtractedAsofSpec,
    view_name: &str,
    right_col_renames: &[(String, String)],
) -> Result<String> {
    let dialect = DuckDbDialect {};
    let mut statements = Parser::parse_sql(&dialect, normalized_sql)
        .map_err(|e| LakehouseError::SqlParse(e.to_string()))?;

    let target_query = match &mut statements[0] {
        Statement::Query(q) => q.as_mut(),
        Statement::Explain { statement, .. } => match statement.as_mut() {
            Statement::Query(q) => q.as_mut(),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };

    let SetExpr::Select(select) = target_query.body.as_mut() else {
        return Ok(normalized_sql.to_string());
    };

    // Replace `FROM L ASOF JOIN R ...` with `FROM <view_name>`
    select.from = vec![TableWithJoins {
        relation: TableFactor::Table {
            name: sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                sqlparser::ast::Ident::new(view_name),
            )]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            partitions: vec![],
            with_ordinality: false,
            json_path: None,
            sample: None,
            index_hints: vec![],
        },
        joins: vec![],
    }];

    let l_qual = spec
        .left_alias
        .as_deref()
        .unwrap_or(&spec.left_relation_sql);
    let r_qual = spec
        .right_alias
        .as_deref()
        .unwrap_or(&spec.right_relation_sql);

    // Rewrite qualified column identifiers `t.col` -> `col` and `q.col` -> renamed right col in SELECT, WHERE, GROUP BY, HAVING, ORDER BY
    for item in &mut select.projection {
        match item {
            SelectItem::UnnamedExpr(e) => {
                // Preserve original column name as alias if right column was disambiguated
                let orig_col = match e {
                    Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                        Some(parts.last().unwrap().value.clone())
                    }
                    _ => None,
                };
                rewrite_qualified_cols_in_expr(e, l_qual, r_qual, right_col_renames);
                if let (Some(orig), Expr::Identifier(curr)) = (orig_col, &*e) {
                    if curr.value != orig {
                        let replaced = e.clone();
                        *item = SelectItem::ExprWithAlias {
                            expr: replaced,
                            alias: sqlparser::ast::Ident::new(orig),
                        };
                    }
                }
            }
            SelectItem::ExprWithAlias { expr, .. } => {
                rewrite_qualified_cols_in_expr(expr, l_qual, r_qual, right_col_renames);
            }
            _ => {}
        }
    }
    if let Some(sel) = &mut select.selection {
        rewrite_qualified_cols_in_expr(sel, l_qual, r_qual, right_col_renames);
    }
    if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &mut select.group_by {
        for e in exprs {
            rewrite_qualified_cols_in_expr(e, l_qual, r_qual, right_col_renames);
        }
    }
    if let Some(hav) = &mut select.having {
        rewrite_qualified_cols_in_expr(hav, l_qual, r_qual, right_col_renames);
    }
    if let Some(order_by) = &mut target_query.order_by {
        if let sqlparser::ast::OrderByKind::Expressions(exprs) = &mut order_by.kind {
            for ob in exprs {
                rewrite_qualified_cols_in_expr(&mut ob.expr, l_qual, r_qual, right_col_renames);
            }
        }
    }

    Ok(statements[0].to_string())
}

fn rewrite_qualified_cols_in_expr(
    expr: &mut Expr,
    l_qual: &str,
    r_qual: &str,
    right_col_renames: &[(String, String)],
) {
    match expr {
        Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
            let qual = &parts[0].value;
            let col = &parts[1].value;
            if qual.eq_ignore_ascii_case(l_qual) {
                *expr = Expr::Identifier(sqlparser::ast::Ident::new(col.clone()));
            } else if qual.eq_ignore_ascii_case(r_qual) {
                let mapped = right_col_renames
                    .iter()
                    .find(|(orig, _)| orig.eq_ignore_ascii_case(col))
                    .map(|(_, renamed)| renamed.clone())
                    .unwrap_or_else(|| col.clone());
                *expr = Expr::Identifier(sqlparser::ast::Ident::new(mapped));
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            rewrite_qualified_cols_in_expr(left, l_qual, r_qual, right_col_renames);
            rewrite_qualified_cols_in_expr(right, l_qual, r_qual, right_col_renames);
        }
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Nested(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull(inner)
        | Expr::IsNotNull(inner) => {
            rewrite_qualified_cols_in_expr(inner, l_qual, r_qual, right_col_renames);
        }
        Expr::Function(func) => {
            if let FunctionArguments::List(list) = &mut func.args {
                for arg in &mut list.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) = arg {
                        rewrite_qualified_cols_in_expr(e, l_qual, r_qual, right_col_renames);
                    }
                }
            }
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            rewrite_qualified_cols_in_expr(e, l_qual, r_qual, right_col_renames);
            rewrite_qualified_cols_in_expr(low, l_qual, r_qual, right_col_renames);
            rewrite_qualified_cols_in_expr(high, l_qual, r_qual, right_col_renames);
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(op) = operand {
                rewrite_qualified_cols_in_expr(op, l_qual, r_qual, right_col_renames);
            }
            for cw in conditions {
                rewrite_qualified_cols_in_expr(&mut cw.condition, l_qual, r_qual, right_col_renames);
                rewrite_qualified_cols_in_expr(&mut cw.result, l_qual, r_qual, right_col_renames);
            }
            if let Some(el) = else_result {
                rewrite_qualified_cols_in_expr(el, l_qual, r_qual, right_col_renames);
            }
        }
        _ => {}
    }
}
