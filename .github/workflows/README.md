# GitHub Actions Workflows

This directory contains CI/CD workflows for the Aspens SDK project.

## Workflows

### CI Workflow (`ci.yml`)

Runs on every push to `main` and `develop` branches, and on all pull requests.

**Jobs:**

1. **Format Check** - Ensures code follows Rust formatting standards
   - Runs `cargo fmt --all -- --check`

2. **Clippy Lints** - Checks for common mistakes and improvements
   - Runs `cargo clippy` on all packages with all features
   - Treats warnings as errors (`-D warnings`)

3. **Build** - Builds all packages
   - Matrix: Ubuntu and macOS, stable Rust
   - Builds: library, CLI, REPL
   - Tests all features including admin feature

4. **Tests** - Runs test suite
   - Runs all tests with and without features

5. **Documentation** - Validates documentation
   - Ensures docs build without warnings
   - Checks all features

6. **Check Dependencies** - Security audit
   - Runs `cargo audit` to check for security vulnerabilities

## Local Development

Before pushing, you can run these checks locally:

```bash
# Format check
cargo fmt --all -- --check

# Apply formatting
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Build all packages
cargo build --workspace --all-features

# Run tests
cargo test --workspace --all-features

# Check documentation
cargo doc --workspace --no-deps --all-features

# Security audit
cargo install cargo-audit
cargo audit
```

## CI Requirements

The CI requires:
- Rust stable toolchain
- Protocol Buffers compiler (`protoc`)
- Standard build tools

All dependencies are automatically installed by the workflow.

## Caching

The workflow uses `Swatinem/rust-cache@v2` to cache:
- Cargo registry
- Cargo index
- Target directory

This significantly speeds up subsequent CI runs.

## Troubleshooting

**If CI fails:**

1. **Format Check Failed**: Run `cargo fmt --all` locally
2. **Clippy Failed**: Run `cargo clippy --workspace --all-targets --all-features` and fix warnings
3. **Build Failed**: Check error messages and ensure proto files are present
4. **Tests Failed**: Run `cargo test --workspace` locally to debug
5. **Docs Failed**: Run `cargo doc --workspace --no-deps` to see doc errors
6. **Audit Failed**: Update dependencies with security fixes

**Common Issues:**

- **Proto files not found**: Ensure proto files are committed to the repository
- **Feature flags**: Make sure all optional dependencies are properly gated
- **Platform-specific issues**: Test on both Linux and macOS if possible
