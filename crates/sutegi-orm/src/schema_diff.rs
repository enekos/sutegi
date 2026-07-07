//! The pure schema **diff engine** — no I/O, no database, just data.
//!
//! Given a *current* set of [`TableSchema`]s (what the database has, reflected
//! by [`Backend::introspect`](crate::Backend::introspect), or a shadow schema
//! folded from prior migrations) and a *desired* set (what the models declare),
//! [`diff`] produces an ordered [`Plan`] of [`SchemaOp`]s that turns one into
//! the other. Every op has a mechanical [`SchemaOp::inverse`], so a generated
//! migration gets its `down` for free, and [`apply`] folds an op back into a
//! schema set — the same interpreter that builds the shadow schema.
//!
//! The design decision that keeps this deterministic: generation always diffs
//! the models against the **replayed migration history**, never against a live
//! database whose state depends on who ran what. The live DB is only consulted
//! for drift detection.
//!
//! Renames are never guessed. A dropped column and an added column of the same
//! storage type surface as a `DropColumn` + `AddColumn` pair plus a
//! [`Plan::warnings`] note — the human rewrites them into a [`SchemaOp::RenameColumn`]
//! if that's what they meant. Guessing renames is how the TypeORM-style
//! generators silently drop data.

use crate::value::{Column, Dialect, ForeignKey, Index, TableSchema};

/// A single, individually-reversible schema change.
#[derive(Clone, Debug, PartialEq)]
pub enum SchemaOp {
    CreateTable(TableSchema),
    /// Carries the full schema (not just the name) so the inverse can recreate
    /// the table structurally — though not its data.
    DropTable(TableSchema),
    AddColumn {
        table: String,
        column: Column,
    },
    /// Carries the dropped column's definition so the inverse re-adds it.
    DropColumn {
        table: String,
        column: Column,
    },
    AlterColumn {
        table: String,
        from: Column,
        to: Column,
    },
    RenameColumn {
        table: String,
        from: String,
        to: String,
    },
    RenameTable {
        from: String,
        to: String,
    },
    CreateIndex {
        table: String,
        index: Index,
    },
    DropIndex {
        table: String,
        index: Index,
    },
    AddForeignKey {
        table: String,
        fk: ForeignKey,
    },
    DropForeignKey {
        table: String,
        fk: ForeignKey,
    },
}

/// How risky an op is to apply to a table that already holds rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Safety {
    /// No data can be lost and the statement can't fail on existing rows.
    Safe,
    /// May destroy data (dropping a table/column, a lossy type change).
    Destructive,
    /// Structurally valid but needs a backfill/default to succeed on a
    /// non-empty table (e.g. adding a `NOT NULL` column with no default, or
    /// tightening a nullable column to `NOT NULL`).
    NeedsData,
}

impl SchemaOp {
    /// The table this op acts on (its *new* name for a rename).
    pub fn table(&self) -> &str {
        match self {
            SchemaOp::CreateTable(s) | SchemaOp::DropTable(s) => &s.table,
            SchemaOp::AddColumn { table, .. }
            | SchemaOp::DropColumn { table, .. }
            | SchemaOp::AlterColumn { table, .. }
            | SchemaOp::RenameColumn { table, .. }
            | SchemaOp::CreateIndex { table, .. }
            | SchemaOp::DropIndex { table, .. }
            | SchemaOp::AddForeignKey { table, .. }
            | SchemaOp::DropForeignKey { table, .. } => table,
            SchemaOp::RenameTable { to, .. } => to,
        }
    }

    /// How risky this op is on a populated table — used to gate dev-mode `sync`
    /// (Safe only) and to flag destructive ops in a generated migration.
    pub fn safety(&self) -> Safety {
        match self {
            SchemaOp::CreateTable(_)
            | SchemaOp::RenameColumn { .. }
            | SchemaOp::RenameTable { .. }
            | SchemaOp::CreateIndex { .. }
            | SchemaOp::AddForeignKey { .. }
            | SchemaOp::DropIndex { .. }
            | SchemaOp::DropForeignKey { .. } => Safety::Safe,

            SchemaOp::DropTable(_) | SchemaOp::DropColumn { .. } => Safety::Destructive,

            SchemaOp::AddColumn { column, .. } => {
                if column.nullable || column.default.is_some() || column.primary {
                    Safety::Safe
                } else {
                    Safety::NeedsData
                }
            }

            SchemaOp::AlterColumn { from, to, .. } => {
                // Type changes may truncate; adding UNIQUE can fail on dup rows.
                if from.ty != to.ty || (!from.unique && to.unique) {
                    Safety::Destructive
                } else if from.nullable && !to.nullable {
                    // Tightening to NOT NULL fails if any existing row is null.
                    Safety::NeedsData
                } else {
                    Safety::Safe
                }
            }
        }
    }

    /// The op that undoes this one. Structural inverse: it restores the schema
    /// shape, not the rows a destructive op removed (a re-added column comes
    /// back empty). Every op is invertible — that's what lets a diff-generated
    /// migration carry a `down` for free.
    pub fn inverse(&self) -> SchemaOp {
        match self {
            SchemaOp::CreateTable(s) => SchemaOp::DropTable(s.clone()),
            SchemaOp::DropTable(s) => SchemaOp::CreateTable(s.clone()),
            SchemaOp::AddColumn { table, column } => SchemaOp::DropColumn {
                table: table.clone(),
                column: column.clone(),
            },
            SchemaOp::DropColumn { table, column } => SchemaOp::AddColumn {
                table: table.clone(),
                column: column.clone(),
            },
            SchemaOp::AlterColumn { table, from, to } => SchemaOp::AlterColumn {
                table: table.clone(),
                from: to.clone(),
                to: from.clone(),
            },
            SchemaOp::RenameColumn { table, from, to } => SchemaOp::RenameColumn {
                table: table.clone(),
                from: to.clone(),
                to: from.clone(),
            },
            SchemaOp::RenameTable { from, to } => SchemaOp::RenameTable {
                from: to.clone(),
                to: from.clone(),
            },
            SchemaOp::CreateIndex { table, index } => SchemaOp::DropIndex {
                table: table.clone(),
                index: index.clone(),
            },
            SchemaOp::DropIndex { table, index } => SchemaOp::CreateIndex {
                table: table.clone(),
                index: index.clone(),
            },
            SchemaOp::AddForeignKey { table, fk } => SchemaOp::DropForeignKey {
                table: table.clone(),
                fk: fk.clone(),
            },
            SchemaOp::DropForeignKey { table, fk } => SchemaOp::AddForeignKey {
                table: table.clone(),
                fk: fk.clone(),
            },
        }
    }

    /// A one-line human summary, for `migrate plan` / generated-file comments.
    pub fn summary(&self) -> String {
        match self {
            SchemaOp::CreateTable(s) => format!("create table {}", s.table),
            SchemaOp::DropTable(s) => format!("drop table {}", s.table),
            SchemaOp::AddColumn { table, column } => {
                format!(
                    "add column {}.{} ({})",
                    table,
                    column.name,
                    column.ty.name()
                )
            }
            SchemaOp::DropColumn { table, column } => {
                format!("drop column {}.{}", table, column.name)
            }
            SchemaOp::AlterColumn { table, from, to } => {
                format!(
                    "alter column {}.{} ({} -> {})",
                    table,
                    from.name,
                    from.ty.name(),
                    to.ty.name()
                )
            }
            SchemaOp::RenameColumn { table, from, to } => {
                format!("rename column {table}.{from} -> {to}")
            }
            SchemaOp::RenameTable { from, to } => format!("rename table {from} -> {to}"),
            SchemaOp::CreateIndex { table, index } => {
                format!("create index {} on {}", index.name, table)
            }
            SchemaOp::DropIndex { table, index } => {
                format!("drop index {} on {}", index.name, table)
            }
            SchemaOp::AddForeignKey { table, fk } => {
                format!(
                    "add fk {}.{} -> {}.{}",
                    table, fk.column, fk.ref_table, fk.ref_column
                )
            }
            SchemaOp::DropForeignKey { table, fk } => {
                format!("drop fk {}.{}", table, fk.column)
            }
        }
    }
}

/// An ordered list of ops plus any advisories the human should read before
/// applying (possible renames, destructive changes, backfill needs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub ops: Vec<SchemaOp>,
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The inverse plan: each op inverted, in reverse order — a ready-made
    /// `down` for a generated migration.
    pub fn inverse(&self) -> Vec<SchemaOp> {
        self.ops.iter().rev().map(SchemaOp::inverse).collect()
    }

    /// True if any op could lose data or fail on a populated table.
    pub fn has_risky_ops(&self) -> bool {
        self.ops.iter().any(|o| o.safety() != Safety::Safe)
    }
}

/// Two columns are equal *for diffing* when they'd render identically in
/// `dialect`: storage type (so `Json`/`Text` don't diff on SQLite), nullability,
/// uniqueness, primary-ness, and default all match.
fn columns_match(a: &Column, b: &Column, dialect: Dialect) -> bool {
    a.ty.storage(dialect) == b.ty.storage(dialect)
        && a.nullable == b.nullable
        && a.unique == b.unique
        && a.primary == b.primary
        && a.default == b.default
}

/// Diff `current` into `desired`, producing the ops (and advisories) that
/// migrate one to the other. Types are compared in `dialect`'s storage terms,
/// so a change that both backends store identically is not reported.
pub fn diff(current: &[TableSchema], desired: &[TableSchema], dialect: Dialect) -> Plan {
    let mut plan = Plan::default();

    let find = |set: &[TableSchema], name: &str| -> Option<TableSchema> {
        set.iter().find(|t| t.table == name).cloned()
    };

    // Dropped tables: in current, gone from desired.
    for cur in current {
        if find(desired, &cur.table).is_none() {
            plan.ops.push(SchemaOp::DropTable(cur.clone()));
            plan.warnings.push(format!(
                "table `{}` is dropped — this destroys its data (down recreates the table empty)",
                cur.table
            ));
        }
    }

    for want in desired {
        let Some(cur) = find(current, &want.table) else {
            // Brand-new table: one CreateTable carries columns, indexes, and FKs.
            plan.ops.push(SchemaOp::CreateTable(want.clone()));
            continue;
        };
        diff_table(&cur, want, dialect, &mut plan);
    }

    plan
}

/// Diff a single table that exists on both sides.
fn diff_table(cur: &TableSchema, want: &TableSchema, dialect: Dialect, plan: &mut Plan) {
    let table = &want.table;

    // --- columns ---
    let added: Vec<&Column> = want
        .columns
        .iter()
        .filter(|c| cur.col(&c.name).is_none())
        .collect();
    let dropped: Vec<&Column> = cur
        .columns
        .iter()
        .filter(|c| want.col(&c.name).is_none())
        .collect();

    // Possible-rename advisory: exactly one add + one drop of the same storage
    // type. We still emit drop+add; the human opts into a RenameColumn.
    if added.len() == 1 && dropped.len() == 1 {
        let (a, d) = (added[0], dropped[0]);
        if a.ty.storage(dialect) == d.ty.storage(dialect) {
            plan.warnings.push(format!(
                "possible rename on `{}`: `{}` dropped, `{}` added (same type) — if this is a \
                 rename, replace the two ops with a RenameColumn to preserve data",
                table, d.name, a.name
            ));
        }
    }

    for c in &dropped {
        plan.ops.push(SchemaOp::DropColumn {
            table: table.clone(),
            column: (*c).clone(),
        });
    }
    for c in &added {
        let op = SchemaOp::AddColumn {
            table: table.clone(),
            column: (*c).clone(),
        };
        if op.safety() == Safety::NeedsData {
            plan.warnings.push(format!(
                "column `{}.{}` is NOT NULL with no default — give it `.default(...)` or add a \
                 backfill migration, or it will fail on a non-empty table",
                table, c.name
            ));
        }
        plan.ops.push(op);
    }
    // Changed columns present on both sides.
    for want_col in &want.columns {
        if let Some(cur_col) = cur.col(&want_col.name) {
            if !columns_match(cur_col, want_col, dialect) {
                let op = SchemaOp::AlterColumn {
                    table: table.clone(),
                    from: cur_col.clone(),
                    to: want_col.clone(),
                };
                match op.safety() {
                    Safety::Destructive => plan.warnings.push(format!(
                        "column `{}.{}` change may lose data or fail on existing rows",
                        table, want_col.name
                    )),
                    Safety::NeedsData => plan.warnings.push(format!(
                        "column `{}.{}` tightened to NOT NULL — existing nulls will fail without a backfill",
                        table, want_col.name
                    )),
                    Safety::Safe => {}
                }
                plan.ops.push(op);
            }
        }
    }

    // --- indexes (matched by name) ---
    for cur_idx in &cur.indexes {
        if !want.indexes.iter().any(|i| i.name == cur_idx.name) {
            plan.ops.push(SchemaOp::DropIndex {
                table: table.clone(),
                index: cur_idx.clone(),
            });
        }
    }
    for want_idx in &want.indexes {
        match cur.indexes.iter().find(|i| i.name == want_idx.name) {
            Some(cur_idx) if cur_idx == want_idx => {}
            Some(cur_idx) => {
                // Same name, different shape: replace.
                plan.ops.push(SchemaOp::DropIndex {
                    table: table.clone(),
                    index: cur_idx.clone(),
                });
                plan.ops.push(SchemaOp::CreateIndex {
                    table: table.clone(),
                    index: want_idx.clone(),
                });
            }
            None => plan.ops.push(SchemaOp::CreateIndex {
                table: table.clone(),
                index: want_idx.clone(),
            }),
        }
    }

    // --- foreign keys (matched by column + referent) ---
    let fk_key = |f: &ForeignKey| (f.column.clone(), f.ref_table.clone(), f.ref_column.clone());
    for cur_fk in &cur.foreign_keys {
        match want
            .foreign_keys
            .iter()
            .find(|f| fk_key(f) == fk_key(cur_fk))
        {
            Some(want_fk) if want_fk.on_delete == cur_fk.on_delete => {}
            _ => plan.ops.push(SchemaOp::DropForeignKey {
                table: table.clone(),
                fk: cur_fk.clone(),
            }),
        }
    }
    for want_fk in &want.foreign_keys {
        match cur
            .foreign_keys
            .iter()
            .find(|f| fk_key(f) == fk_key(want_fk))
        {
            Some(cur_fk) if cur_fk.on_delete == want_fk.on_delete => {}
            _ => plan.ops.push(SchemaOp::AddForeignKey {
                table: table.clone(),
                fk: want_fk.clone(),
            }),
        }
    }
}

/// Fold one op into a schema set — the pure interpreter behind both the shadow
/// schema and the round-trip property test. Errors if an op references a table
/// that isn't there (a malformed migration).
pub fn apply(schemas: &mut Vec<TableSchema>, op: &SchemaOp) -> Result<(), String> {
    let table_mut = |schemas: &mut Vec<TableSchema>, name: &str| -> Result<usize, String> {
        schemas
            .iter()
            .position(|t| t.table == name)
            .ok_or_else(|| format!("apply: no such table `{name}`"))
    };

    match op {
        SchemaOp::CreateTable(s) => {
            if schemas.iter().any(|t| t.table == s.table) {
                return Err(format!("apply: table `{}` already exists", s.table));
            }
            schemas.push(s.clone());
        }
        SchemaOp::DropTable(s) => {
            let i = table_mut(schemas, &s.table)?;
            schemas.remove(i);
        }
        SchemaOp::AddColumn { table, column } => {
            let i = table_mut(schemas, table)?;
            schemas[i].columns.push(column.clone());
        }
        SchemaOp::DropColumn { table, column } => {
            let i = table_mut(schemas, table)?;
            schemas[i].columns.retain(|c| c.name != column.name);
        }
        SchemaOp::AlterColumn { table, to, .. } => {
            let i = table_mut(schemas, table)?;
            if let Some(c) = schemas[i].columns.iter_mut().find(|c| c.name == to.name) {
                *c = to.clone();
            } else {
                return Err(format!("apply: no such column `{}.{}`", table, to.name));
            }
        }
        SchemaOp::RenameColumn { table, from, to } => {
            let i = table_mut(schemas, table)?;
            let t = &mut schemas[i];
            let found = t.columns.iter_mut().find(|c| c.name == *from);
            match found {
                Some(c) => c.name = to.clone(),
                None => return Err(format!("apply: no such column `{table}.{from}`")),
            }
            // Keep indexes and FKs on the renamed column consistent.
            for idx in &mut t.indexes {
                for col in &mut idx.columns {
                    if col == from {
                        *col = to.clone();
                    }
                }
            }
            for fk in &mut t.foreign_keys {
                if fk.column == *from {
                    fk.column = to.clone();
                }
            }
        }
        SchemaOp::RenameTable { from, to } => {
            let i = table_mut(schemas, from)?;
            schemas[i].table = to.clone();
            // Repoint any FK that referenced the old table name.
            for t in schemas.iter_mut() {
                for fk in &mut t.foreign_keys {
                    if fk.ref_table == *from {
                        fk.ref_table = to.clone();
                    }
                }
            }
        }
        SchemaOp::CreateIndex { table, index } => {
            let i = table_mut(schemas, table)?;
            schemas[i].indexes.push(index.clone());
        }
        SchemaOp::DropIndex { table, index } => {
            let i = table_mut(schemas, table)?;
            schemas[i].indexes.retain(|x| x.name != index.name);
        }
        SchemaOp::AddForeignKey { table, fk } => {
            let i = table_mut(schemas, table)?;
            schemas[i].foreign_keys.push(fk.clone());
        }
        SchemaOp::DropForeignKey { table, fk } => {
            let i = table_mut(schemas, table)?;
            schemas[i]
                .foreign_keys
                .retain(|f| !(f.column == fk.column && f.ref_table == fk.ref_table));
        }
    }
    Ok(())
}

/// Fold a whole op list into a schema set, in order.
pub fn apply_all(schemas: &mut Vec<TableSchema>, ops: &[SchemaOp]) -> Result<(), String> {
    for op in ops {
        apply(schemas, op)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ColType, FkAction};

    fn users() -> TableSchema {
        TableSchema::new("users")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("email", ColType::Text).unique())
    }

    fn posts() -> TableSchema {
        TableSchema::new("posts")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
            .column(Column::new("user_id", ColType::Integer))
            .foreign_key("user_id", "users", "id", FkAction::Cascade)
            .index(&["user_id"])
    }

    /// Every op composed with its inverse is the identity op.
    #[test]
    fn inverse_is_an_involution() {
        let ops = vec![
            SchemaOp::CreateTable(posts()),
            SchemaOp::AddColumn {
                table: "posts".into(),
                column: Column::new("views", ColType::Integer).default(crate::Value::Int(0)),
            },
            SchemaOp::AlterColumn {
                table: "posts".into(),
                from: Column::new("title", ColType::Text),
                to: Column::new("title", ColType::Text).nullable(),
            },
            SchemaOp::RenameColumn {
                table: "posts".into(),
                from: "title".into(),
                to: "headline".into(),
            },
            SchemaOp::CreateIndex {
                table: "posts".into(),
                index: Index {
                    name: "idx_posts_user_id".into(),
                    columns: vec!["user_id".into()],
                    unique: false,
                },
            },
        ];
        for op in &ops {
            assert_eq!(
                &op.inverse().inverse(),
                op,
                "inverse∘inverse ≠ id for {op:?}"
            );
        }
    }

    /// The central guarantee: applying `diff(a, b)` to `a` yields `b`.
    #[test]
    fn fold_of_diff_reconstructs_target() {
        let cases: Vec<(Vec<TableSchema>, Vec<TableSchema>)> = vec![
            // create from nothing
            (vec![], vec![users(), posts()]),
            // drop a table
            (vec![users(), posts()], vec![users()]),
            // add / drop / alter columns + add an index + a default
            (
                vec![users()],
                vec![TableSchema::new("users")
                    .column(Column::new("id", ColType::Integer).primary())
                    .column(Column::new("email", ColType::Text).unique())
                    .column(
                        Column::new("name", ColType::Text).default(crate::Value::Text("".into())),
                    )
                    .index(&["email"])],
            ),
            // change a foreign key's on_delete
            (
                vec![users(), posts()],
                vec![
                    users(),
                    TableSchema::new("posts")
                        .column(Column::new("id", ColType::Integer).primary())
                        .column(Column::new("title", ColType::Text))
                        .column(Column::new("user_id", ColType::Integer))
                        .foreign_key("user_id", "users", "id", FkAction::SetNull)
                        .index(&["user_id"]),
                ],
            ),
        ];

        for (a, b) in cases {
            let plan = diff(&a, &b, Dialect::Sqlite);
            let mut folded = a.clone();
            apply_all(&mut folded, &plan.ops).unwrap();
            let norm = |v: Vec<TableSchema>| {
                let mut v: Vec<TableSchema> = v.iter().map(|t| t.normalized()).collect();
                v.sort_by(|x, y| x.table.cmp(&y.table));
                v
            };
            assert_eq!(norm(folded), norm(b.clone()), "fold(diff(a,b),a) != b");

            // And the round trip: applying the inverse gets back to `a`.
            let mut back = norm(b);
            apply_all(&mut back, &plan.inverse()).unwrap();
            assert_eq!(norm(back), norm(a), "inverse plan did not restore a");
        }
    }

    #[test]
    fn identical_schemas_produce_empty_plan() {
        let plan = diff(&[users(), posts()], &[users(), posts()], Dialect::Sqlite);
        assert!(plan.is_empty(), "expected no ops, got {:?}", plan.ops);
    }

    #[test]
    fn json_vs_text_does_not_diff_on_sqlite() {
        // Both store as TEXT on SQLite, so no change should be reported.
        let a = vec![TableSchema::new("t").column(Column::new("meta", ColType::Text))];
        let b = vec![TableSchema::new("t").column(Column::new("meta", ColType::Json))];
        assert!(diff(&a, &b, Dialect::Sqlite).is_empty());
        // On Postgres they differ (TEXT vs JSONB) → one AlterColumn.
        let pg = diff(&a, &b, Dialect::Postgres);
        assert_eq!(pg.ops.len(), 1);
        assert!(matches!(pg.ops[0], SchemaOp::AlterColumn { .. }));
    }

    #[test]
    fn rename_shaped_change_warns_but_keeps_drop_add() {
        let a = vec![TableSchema::new("t")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))];
        let b = vec![TableSchema::new("t")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("headline", ColType::Text))];
        let plan = diff(&a, &b, Dialect::Sqlite);
        // Never guesses: emits both a drop and an add.
        assert!(plan
            .ops
            .iter()
            .any(|o| matches!(o, SchemaOp::DropColumn { .. })));
        assert!(plan
            .ops
            .iter()
            .any(|o| matches!(o, SchemaOp::AddColumn { .. })));
        assert!(plan.warnings.iter().any(|w| w.contains("possible rename")));
    }

    #[test]
    fn not_null_without_default_is_flagged_needs_data() {
        let a = vec![TableSchema::new("t").column(Column::new("id", ColType::Integer).primary())];
        let b = vec![TableSchema::new("t")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("qty", ColType::Integer))];
        let plan = diff(&a, &b, Dialect::Sqlite);
        let add = plan
            .ops
            .iter()
            .find(|o| matches!(o, SchemaOp::AddColumn { .. }))
            .unwrap();
        assert_eq!(add.safety(), Safety::NeedsData);
        assert!(plan.warnings.iter().any(|w| w.contains("NOT NULL")));
    }
}
