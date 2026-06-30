.PHONY: build test run release size bench clean

build:
	cargo build

test:
	cargo test

# Run the demo app (override port: make run ADDR=127.0.0.1:9000)
ADDR ?= 127.0.0.1:8080
run:
	cargo run -p todo-example -- $(ADDR)

release:
	cargo build --release

# Report the size-optimized binary sizes.
size: release
	@for b in todo sutegi; do printf "%-8s %8d bytes\n" "$$b" "$$(stat -f%z target/release/$$b 2>/dev/null || stat -c%s target/release/$$b)"; done

# Statistical microbenchmarks via the aatxe SDK (emits a RunReport on stdout).
# Requires the aatxe repo cloned as a sibling (../aatxe).
bench:
	cd benches && cargo run --release --bin sutegi-bench

clean:
	cargo clean
