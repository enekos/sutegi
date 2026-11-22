-- Run once in a fresh database as an administrator after installing the module.
BEGIN;
CREATE SCHEMA warehouse;
CREATE TABLE warehouse.stock (
  sku text PRIMARY KEY,
  available integer NOT NULL CHECK (available >= 0)
);
INSERT INTO warehouse.stock VALUES ('TEA', 10), ('COFFEE', 2);

CREATE SCHEMA inventory_api;
CREATE EXTENSION pgext_inventory WITH SCHEMA inventory_api;

CREATE ROLE inventory_writer NOLOGIN;
CREATE ROLE order_clerk NOLOGIN;
CREATE ROLE stock_reader NOLOGIN;
GRANT USAGE ON SCHEMA warehouse TO inventory_writer, stock_reader;
GRANT SELECT (sku, available), UPDATE (available) ON warehouse.stock TO inventory_writer;
GRANT SELECT ON warehouse.stock TO stock_reader;
ALTER FUNCTION inventory_api.reserve_stock(text, integer) OWNER TO inventory_writer;
GRANT USAGE ON SCHEMA inventory_api TO order_clerk, stock_reader;
GRANT EXECUTE ON FUNCTION inventory_api.reserve_stock(text, integer) TO order_clerk;
COMMIT;
