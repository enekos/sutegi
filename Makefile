AATXE_BIN ?= aatxe
AATXE_CLI_VERSION := 0.2.0

.PHONY: build test run release size install-hooks install-bench-cli check-bench-cli bench bench-baseline bench-compare clean

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
# Set SUTEGI_PG_TEST_URL to include the pg_* benches.
bench:
	cd benches && cargo run --release --bin sutegi-bench

# Re-capture the committed baseline RunReport (run on a quiet machine, from
# the commit you want to measure against).
bench-baseline:
	cd benches && cargo run --release --bin sutegi-bench > baselines/local.json
	@echo "baseline written to benches/baselines/local.json"

install-bench-cli:
	cargo install aatxe --locked --version '=$(AATXE_CLI_VERSION)'

check-bench-cli:
	@version=$$("$(AATXE_BIN)" --version) && [ "$$version" = "aatxe $(AATXE_CLI_VERSION)" ] || { \
		echo "Expected aatxe $(AATXE_CLI_VERSION). Run make install-bench-cli and check AATXE_BIN or PATH." >&2; exit 1; }

# Statistical perf gate: bench the working tree and compare it against the
# committed baseline via the aatxe CLI (median Δ + Mann-Whitney U + noise gate).
# Exits 2 if anything regressed. AATXE_BIN selects the CLI (default: aatxe).
bench-compare: check-bench-cli
	cd benches && cargo run --release --bin sutegi-bench > /tmp/sutegi-bench-head.json
	"$(AATXE_BIN)" compare --base benches/baselines/local.json --head /tmp/sutegi-bench-head.json \
		--out /tmp/sutegi-bench-compare.json --markdown /tmp/sutegi-bench-compare.md \
		--fail-on-regression; \
	status=$$?; cat /tmp/sutegi-bench-compare.md; exit $$status

# Point git at the tracked hooks directory so pre-commit checks run on every commit.
install-hooks:
	git config core.hooksPath .githooks
	@echo "git hooks installed from .githooks"

clean:
	cargo clean
