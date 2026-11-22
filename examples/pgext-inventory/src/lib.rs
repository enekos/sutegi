use sutegi_pgext::{Error, Result, SetOf, SqlState, pg_function, spi};

/// Reserve units and return the remaining stock. NULL means the order could not be filled.
#[pg_function(volatile, security_definer)]
fn reserve_stock(sku: &str, units: i32) -> Result<Option<i32>> {
    if units < 1 {
        return Err(Error::new(
            SqlState::INVALID_PARAMETER_VALUE,
            "units must be positive",
        ));
    }
    let remaining = spi::query_scalar(
        "UPDATE warehouse.stock SET available = available - $2
         WHERE sku = $1 AND available >= $2 RETURNING available",
        &[&sku, &units],
    )?;
    Ok(remaining.map(|n| n.parse()).transpose()?)
}

/// Up to 50 SKUs at or below the threshold, lowest stock first.
#[pg_function(stable, rows = 50)]
fn low_stock(threshold: i32) -> Result<SetOf<String>> {
    let rows = spi::query(
        "SELECT sku FROM warehouse.stock WHERE available <= $1
         ORDER BY available, sku LIMIT 50",
        &[&threshold],
    )?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get(0).map(str::to_owned))
        .collect())
}
