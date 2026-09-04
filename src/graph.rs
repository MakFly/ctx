use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use rusqlite::{Connection, params_from_iter};

use crate::config::find_ctx;
use crate::db::connect;
use crate::model::{Envelope, Hit};
use crate::search::apply_budget;

#[derive(Debug)]
struct Definition {
    id: i64,
    hit: Hit,
}

pub fn graph_query(
    operation: &str,
    symbol: &str,
    depth: usize,
    budget_tokens: usize,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let started = Instant::now();
    let connection = connect(&find_ctx(start)?.join("index.sqlite"), false)?;
    let definitions = definitions(&connection, symbol)?;
    let mut coverage = "complete";
    let mut hint = None;
    let mut hits = match operation {
        "def" => definitions.iter().map(|item| item.hit.clone()).collect(),
        "refs" | "callers" => {
            if definitions.is_empty() || definitions.len() > 1 {
                coverage = "partial";
                hint = Some("résolution statique best-effort".to_owned());
            }
            references(&connection, symbol, &definitions)?
        }
        "callees" => {
            coverage = "partial";
            hint = Some("résolution statique best-effort".to_owned());
            callees(&connection, &definitions)?
        }
        "path" | "impact" => {
            coverage = "partial";
            hint = Some(format!(
                "résolution statique best-effort; profondeur demandée {depth}"
            ));
            let mut values = definitions
                .iter()
                .map(|item| item.hit.clone())
                .collect::<Vec<_>>();
            values.extend(references(&connection, symbol, &definitions)?);
            values
        }
        _ => bail!("opération graph inconnue: {operation}"),
    };
    if operation == "def" && definitions.len() > 1 {
        coverage = "partial";
        hint = Some("plusieurs définitions correspondent".to_owned());
    }
    let mut seen = HashSet::new();
    hits.retain(|hit| seen.insert((hit.path.clone(), hit.start, hit.kind.clone())));
    Ok(apply_budget(hits, budget_tokens, started, coverage, hint))
}

fn definitions(connection: &Connection, symbol: &str) -> Result<Vec<Definition>> {
    let mut statement = connection.prepare(
        "SELECT s.id,f.path,s.start,s.end,s.name,COALESCE(s.sig,''),
                COALESCE(s.snippet,''),f.is_test
         FROM symbols s JOIN files f ON f.id=s.file_id
         WHERE s.name=?1 OR s.qualname=?1
         ORDER BY f.is_test,f.is_vendor,f.path,s.start",
    )?;
    let rows = statement.query_map([symbol], |row| {
        Ok(Definition {
            id: row.get(0)?,
            hit: Hit {
                path: row.get(1)?,
                start: row.get::<_, i64>(2)?.max(1) as usize,
                end: row.get::<_, i64>(3)?.max(1) as usize,
                symbol: Some(row.get(4)?),
                kind: if row.get::<_, bool>(7)? {
                    "test"
                } else {
                    "def"
                }
                .to_owned(),
                sig: row.get(5)?,
                snippet: row.get(6)?,
                score: 1.0,
                why: "definition".to_owned(),
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn references(
    connection: &Connection,
    symbol: &str,
    definitions: &[Definition],
) -> Result<Vec<Hit>> {
    let ids = definitions.iter().map(|item| item.id).collect::<Vec<_>>();
    let (sql, values): (String, Vec<rusqlite::types::Value>) = if ids.is_empty() {
        (
            "SELECT e.line,e.kind,e.source,e.confidence,f.path,f.is_test,
                    COALESCE(s.name,e.dst_name),COALESCE(s.start,e.line),
                    COALESCE(s.end,e.line),COALESCE(s.sig,''),COALESCE(s.snippet,'')
             FROM edges e JOIN files f ON f.id=e.file_id
             LEFT JOIN symbols s ON s.id=e.src_symbol_id
             WHERE e.dst_name=?1
             ORDER BY (e.source='lsp') DESC,f.is_test,f.path,e.line"
                .to_owned(),
            vec![symbol.to_owned().into()],
        )
    } else {
        let placeholders = (1..=ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let symbol_parameter = ids.len() + 1;
        let sql = format!(
            "SELECT e.line,e.kind,e.source,e.confidence,f.path,f.is_test,
                    COALESCE(s.name,e.dst_name),COALESCE(s.start,e.line),
                    COALESCE(s.end,e.line),COALESCE(s.sig,''),COALESCE(s.snippet,'')
             FROM edges e JOIN files f ON f.id=e.file_id
             LEFT JOIN symbols s ON s.id=e.src_symbol_id
             WHERE e.dst_symbol_id IN ({placeholders}) OR e.dst_name=?{symbol_parameter}
             ORDER BY (e.source='lsp') DESC,f.is_test,f.path,e.line"
        );
        let mut values = ids
            .iter()
            .copied()
            .map(rusqlite::types::Value::Integer)
            .collect::<Vec<_>>();
        values.push(symbol.to_owned().into());
        (sql, values)
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| {
        let edge_kind = row.get::<_, String>(1)?;
        let source = row.get::<_, String>(2)?;
        Ok(Hit {
            path: row.get(4)?,
            start: row.get::<_, i64>(7)?.max(1) as usize,
            end: row.get::<_, i64>(8)?.max(1) as usize,
            symbol: Some(row.get(6)?),
            kind: if edge_kind == "call" { "call" } else { "ref" }.to_owned(),
            sig: row.get(9)?,
            snippet: row.get(10)?,
            score: if source == "lsp" { row.get(3)? } else { 0.9 },
            why: format!("{source} {edge_kind} of {symbol}"),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn callees(connection: &Connection, definitions: &[Definition]) -> Result<Vec<Hit>> {
    if definitions.is_empty() {
        return Ok(Vec::new());
    }
    let ids = definitions.iter().map(|item| item.id).collect::<Vec<_>>();
    let placeholders = (1..=ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT d.name,d.start,d.end,COALESCE(d.sig,''),COALESCE(d.snippet,''),f.path
         FROM edges e JOIN symbols d ON d.id=e.dst_symbol_id
         JOIN files f ON f.id=d.file_id
         WHERE e.src_symbol_id IN ({placeholders})
         ORDER BY f.path,d.start"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(Hit {
            path: row.get(5)?,
            start: row.get::<_, i64>(1)?.max(1) as usize,
            end: row.get::<_, i64>(2)?.max(1) as usize,
            symbol: Some(row.get(0)?),
            kind: "call".to_owned(),
            sig: row.get(3)?,
            snippet: row.get(4)?,
            score: 0.9,
            why: "callee".to_owned(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
