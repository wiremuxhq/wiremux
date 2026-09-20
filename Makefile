.PHONY: help check public

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

check: ## fmt, clippy, test, deny, public surfaces, trigger lock
	cargo fmt --check
	RUSTFLAGS="-D warnings" cargo clippy --locked --workspace --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --workspace
	RUSTFLAGS="-D warnings" cargo test --locked --workspace
	RUSTFLAGS="-D warnings" cargo test --locked -p wiremux --no-default-features --test request_maps --test stream_maps --test response_maps --test stream_grammar
	RUSTFLAGS="-D warnings" cargo clippy --locked -p wiremux --all-targets --no-default-features --features client -- -D warnings
	RUSTFLAGS="-D warnings" cargo test --locked -p wiremux --no-default-features --features client --test client --test consume_notes --test response_maps
	cargo check --locked -p wiremux --no-default-features --features proxy
	bash scripts/assert-maps-only-deps.sh
	bash scripts/check-cargo-package.sh
	cargo deny check
	python3 scripts/test_workflow_triggers.py
	bash scripts/assert-public.sh

public: ## Check launch surfaces in the tree
	bash scripts/assert-public.sh
