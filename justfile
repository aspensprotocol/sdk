#!/usr/bin/env just --justfile

# List all available commands
default:
    @just --list

# Set up the development environment
init:
    #!/usr/bin/env bash
    cp .env.sample .env
    echo "Please edit the created .env with your account values"

# Build the entire workspace
build:
    cargo build

# Build workspace in release mode
release:
    cargo build --release

# Build only the core library
build-lib:
    cargo build -p aspens

# Build only the CLI
build-cli:
    cargo build -p aspens-cli

# Build only the REPL
build-repl:
    cargo build -p aspens-repl

# Build only the Admin 
build-admin:
    cargo build -p aspens-admin

# Run tests for the entire workspace
test:
    cargo test

# Run tests for core library only
test-lib:
    cargo test -p aspens

# Clean build artifacts
clean:
    #!/usr/bin/env bash
    cargo clean
    rm -rf target
    rm -rf anvil*.log
    rm -rf artifacts/

# Format code for the entire workspace
fmt:
    cargo fmt --all

# Check code style for the entire workspace
check:
    cargo check --workspace

# Run linter on the entire workspace
lint:
    cargo clippy --workspace

# Run AMMIT tests with specific environment
test-anvil:
    ./scripts/ammit.sh anvil

test-testnet:
    ./scripts/ammit.sh testnet

# Update JWT token in .env file (requires running Aspens Market Stack)
update-jwt:
    #!/usr/bin/env bash
    source .env
    ADDRESS=$(cast wallet address $ADMIN_PRIVKEY | tr '[:upper:]' '[:lower:]')

    echo "Attempting to obtain JWT for admin address: $ADDRESS"

    # Try init-admin first (in case server was reset)
    OUTPUT=$(cargo run -p aspens-admin -- init-admin --address $ADDRESS 2>&1) || true

    if echo "$OUTPUT" | grep -q "JWT Token:"; then
        echo "✓ Admin initialized"
        JWT=$(echo "$OUTPUT" | grep "JWT Token:" | awk '{print $3}')
    elif echo "$OUTPUT" | grep -q "Admin already initialized"; then
        # Admin exists, try login
        echo "Admin already exists, attempting login..."
        OUTPUT=$(cargo run -p aspens-admin -- login 2>&1) || true

        if echo "$OUTPUT" | grep -q "JWT Token:"; then
            echo "✓ Login successful"
            JWT=$(echo "$OUTPUT" | grep "JWT Token:" | awk '{print $3}')
        else
            echo ""
            echo "❌ Cannot obtain JWT - server admin state is inconsistent"
            echo ""
            echo "The server has an admin initialized but login failed."
            echo "This usually happens when:"
            echo "  1. JWT expired and server was restarted with different admin"
            echo "  2. Server database has stale admin data"
            echo ""
            echo "To fix this, restart your Aspens Market Stack server with a clean state."
            echo "The server should reset admin state on fresh startup."
            exit 1
        fi
    else
        echo "Error communicating with server:"
        echo "$OUTPUT"
        exit 1
    fi

    if [ -z "$JWT" ]; then
        echo "Error: Failed to extract JWT from output"
        exit 1
    fi

    # Update .env file (macOS-compatible)
    if [[ "$OSTYPE" == "darwin"* ]]; then
        sed -i '' "s|^ASPENS_JWT=.*|ASPENS_JWT=$JWT|" .env
    else
        sed -i "s|^ASPENS_JWT=.*|ASPENS_JWT=$JWT|" .env
    fi

    echo ""
    echo "✓ JWT updated successfully in .env"
    echo "New JWT: $JWT"
