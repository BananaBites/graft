APP := graft
CARGO := cargo

.PHONY: run build release test check fmt clean install next-version

# Run locally. Pass args with: make run ARGS="/path/to/repo --delta-dark"
run:
	$(CARGO) run -- $(ARGS)

# Fast debug build.
build:
	$(CARGO) build

# Optimized release build: target/release/$(APP)
release:
	$(CARGO) build --release

# Run unit tests.
test:
	$(CARGO) test

# Type-check without producing a binary.
check:
	$(CARGO) check

# Format Rust sources.
fmt:
	$(CARGO) fmt

# Remove build artifacts.
clean:
	$(CARGO) clean

# Install this checkout into ~/.cargo/bin/$(APP).
install:
	$(CARGO) install --path .

# Suggest the next patch tag from existing vMAJOR.MINOR.PATCH tags.
next-version:
	@latest=$$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$$' | tail -n 1); \
	if [ -z "$$latest" ]; then \
		next=v0.1.0; \
	else \
		version=$${latest#v}; \
		major=$${version%%.*}; \
		rest=$${version#*.}; \
		minor=$${rest%%.*}; \
		patch=$${rest#*.}; \
		next=v$$major.$$minor.$$((patch + 1)); \
	fi; \
	echo "latest tag: $${latest:-none}"; \
	echo "suggested next version: $$next"; \
	echo; \
	echo "git tag -a $$next -m \"$$next\""; \
	echo "git push origin $$next"
