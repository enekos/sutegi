\ir setup.sql

SET ROLE stock_reader;
DO $$
BEGIN
  ASSERT NOT has_function_privilege(current_user, 'inventory_api.reserve_stock(text,integer)', 'EXECUTE');
  BEGIN
    PERFORM inventory_api.reserve_stock('TEA', 1);
    RAISE 'stock_reader could reserve stock';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
  ASSERT (SELECT array_agg(sku ORDER BY pos)
          FROM inventory_api.low_stock(10) WITH ORDINALITY AS r(sku, pos)) = ARRAY['COFFEE', 'TEA'];
  ASSERT (SELECT count(*) FROM inventory_api.low_stock(1)) = 0;
END $$;
RESET ROLE;

INSERT INTO warehouse.stock VALUES ('Kid''s mug', 2);
SET ROLE order_clerk;
BEGIN;
DO $$
BEGIN
  ASSERT NOT has_schema_privilege(current_user, 'warehouse', 'USAGE');
  BEGIN
    PERFORM inventory_api.low_stock(10);
    RAISE 'order_clerk could read warehouse.stock';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
  ASSERT inventory_api.reserve_stock('TEA', 3) = 7;
  ASSERT inventory_api.reserve_stock('TEA', 8) IS NULL;
  ASSERT inventory_api.reserve_stock('missing', 1) IS NULL;
  ASSERT inventory_api.reserve_stock('Kid''s mug', 2) = 0;
  BEGIN
    PERFORM inventory_api.reserve_stock('TEA', 0);
    RAISE 'zero units accepted';
  EXCEPTION WHEN invalid_parameter_value THEN NULL;
  END;
END $$;
ROLLBACK;

-- A reservation participates in its caller's transaction.
SELECT inventory_api.reserve_stock('TEA', 10);
RESET ROLE;
DO $$
BEGIN
  ASSERT (SELECT available FROM warehouse.stock WHERE sku = 'TEA') = 0;
  ASSERT (SELECT available FROM warehouse.stock WHERE sku = 'Kid''s mug') = 2;

  INSERT INTO warehouse.stock SELECT 'SKU-' || n, 1 FROM generate_series(1, 70) AS n;
  ASSERT (SELECT count(*) FROM inventory_api.low_stock(10)) = 50;
END $$;
