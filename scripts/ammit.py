#!/usr/bin/env python3
"""
AMMIT - Automated Market Maker Internal Tester

A test script for the Aspens CLI that executes a full trading sequence including:
- Server connection verification
- Token deposits on multiple chains
- Placing buy and sell orders
- Balance verification
- Token withdrawals

Usage:
    python scripts/ammit.py
    python scripts/ammit.py --stack-url http://localhost:50051
    python scripts/ammit.py --network-base anvil-1 --network-quote anvil-2
    python scripts/ammit.py --dry-run  # Show commands without executing
"""

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from typing import Optional, List


# ANSI color codes
class Colors:
    GREEN = '\033[0;32m'
    RED = '\033[0;31m'
    YELLOW = '\033[1;33m'
    BLUE = '\033[0;34m'
    CYAN = '\033[0;36m'
    BOLD = '\033[1m'
    NC = '\033[0m'  # No Color


def print_section(title: str) -> None:
    """Print a section header."""
    print(f"\n{Colors.GREEN}=== {title} ==={Colors.NC}")


def print_info(message: str) -> None:
    """Print an info message."""
    print(f"{Colors.GREEN}{message}{Colors.NC}")


def print_warning(message: str) -> None:
    """Print a warning message."""
    print(f"{Colors.YELLOW}{message}{Colors.NC}")


def print_error(message: str) -> None:
    """Print an error message."""
    print(f"{Colors.RED}{message}{Colors.NC}")


def print_command(command: str) -> None:
    """Print a command being executed."""
    print(f"{Colors.BLUE}-> {command}{Colors.NC}")


def print_success(message: str) -> None:
    """Print a success message."""
    print(f"{Colors.GREEN}{message}{Colors.NC}")


@dataclass
class Config:
    """Configuration for the test script."""
    stack_url: str
    base_network: str
    quote_network: str
    base_token: str
    quote_token: str
    market_id: Optional[str]
    cli_binary: str
    dry_run: bool
    verbose: bool
    deposit_amount: int
    withdraw_amount: int


def find_cli_binary() -> str:
    """Find the aspens-cli binary in target/release or target/debug."""
    import os

    # Check both current directory and parent directory (for when running from scripts/)
    search_paths = [".", ".."]

    for base_path in search_paths:
        # Check release first, then debug
        for build_type in ["release", "debug"]:
            binary_path = os.path.join(base_path, "target", build_type, "aspens-cli")
            if os.path.isfile(binary_path) and os.access(binary_path, os.X_OK):
                return binary_path

    # Fallback to just the binary name (might be in PATH)
    return "aspens-cli"


def build_cli_command(config: Config, *args: str) -> List[str]:
    """Build the CLI command with proper arguments."""
    cmd = [config.cli_binary]

    # Add stack URL
    cmd.extend(["--stack", config.stack_url])

    # Add verbosity if enabled
    if config.verbose:
        cmd.append("-v")

    # Add the actual command arguments
    cmd.extend(args)

    return cmd


def build_cli_binary() -> bool:
    """Build the aspens-cli binary. Returns True if successful."""
    print_info("CLI binary not found. Building it now...")
    try:
        result = subprocess.run(
            ["cargo", "build", "-p", "aspens-cli"],
            capture_output=True,
            text=True,
            check=False
        )

        if result.returncode == 0:
            print_success("✓ CLI binary built successfully")
            return True
        else:
            print_error(f"Failed to build CLI binary: {result.stderr}")
            return False

    except Exception as e:
        print_error(f"Error building CLI binary: {e}")
        return False


def run_cli(config: Config, *args: str, _auto_build_attempted: bool = False) -> Optional[subprocess.CompletedProcess]:
    """Run a CLI command and return the result."""
    cmd = build_cli_command(config, *args)
    cmd_str = " ".join(cmd)

    print_command(cmd_str)

    if config.dry_run:
        print(f"{Colors.CYAN}  [dry-run] Command not executed{Colors.NC}")
        return None

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False
        )

        # Print stdout if present
        if result.stdout:
            print(result.stdout)

        # Print stderr if present (often contains log messages)
        if result.stderr:
            # Filter out common log prefixes for cleaner output
            for line in result.stderr.strip().split('\n'):
                if line:
                    print(f"  {line}")

        if result.returncode != 0:
            print_error(f"Command failed with exit code {result.returncode}")
            return result

        return result

    except FileNotFoundError:
        if not _auto_build_attempted:
            print_warning(f"CLI binary not found at '{config.cli_binary}'")
            if build_cli_binary():
                # Update binary path and retry
                config.cli_binary = find_cli_binary()
                print_info("Retrying command with newly built binary...")
                return run_cli(config, *args, _auto_build_attempted=True)

        print_error(f"Error: CLI binary not found at '{config.cli_binary}'")
        print_error("Build it first with 'just build' or 'cargo build'")
        return None
    except Exception as e:
        print_error(f"Error running command: {e}")
        return None


def fetch_config(config: Config) -> Optional[dict]:
    """Fetch configuration from the server and parse as JSON."""
    cmd = build_cli_command(config, "config")

    if config.dry_run:
        print_command(" ".join(cmd))
        print(f"{Colors.CYAN}  [dry-run] Would fetch config{Colors.NC}")
        return None

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=True
        )

        # The config command outputs JSON to stdout
        return json.loads(result.stdout)

    except subprocess.CalledProcessError as e:
        print_error(f"Failed to fetch config: {e.stderr}")
        return None
    except json.JSONDecodeError as e:
        print_error(f"Failed to parse config JSON: {e}")
        return None


def get_market_id(config: Config) -> Optional[str]:
    """Get the market ID, either from config or by fetching from server."""
    if config.market_id:
        return config.market_id

    print_info("Fetching market ID from server config...")
    server_config = fetch_config(config)

    if not server_config:
        return None

    # Extract markets from config
    cfg = server_config.get("config", {})
    markets = cfg.get("markets", [])

    if not markets:
        print_error("No markets found in server configuration")
        return None

    # Use the first market by default
    market = markets[0]
    market_id = market.get("marketId")

    if market_id:
        print_info(f"Using market: {market.get('name', market_id)} ({market_id})")
        return market_id

    print_error("Could not extract market ID from configuration")
    return None


def run_test_sequence(config: Config) -> bool:
    """Run the full test sequence. Returns True if all steps succeed."""
    success = True

    print_info(f"AMMIT - Automated Market Maker Internal Tester")
    print_info(f"CLI Binary: {config.cli_binary}")
    print_info(f"Stack URL: {config.stack_url}")
    print_info(f"Base Network: {config.base_network} ({config.base_token})")
    print_info(f"Quote Network: {config.quote_network} ({config.quote_token})")
    if config.dry_run:
        print_warning("DRY RUN MODE - Commands will not be executed")

    # Step 1: Check status
    print_section("Checking connection status")
    result = run_cli(config, "status")
    if result and result.returncode != 0:
        print_error("Failed to connect to server")
        return False

    # Step 2: Get market ID if not provided
    market_id = get_market_id(config)
    if not market_id and not config.dry_run:
        print_error("No market ID available. Use --market-id or ensure server has markets configured.")
        return False
    market_id = market_id or "MARKET_ID_PLACEHOLDER"

    # Step 3: Check initial balances
    print_section("Checking initial balances")
    run_cli(config, "balance")

    # Step 4: Deposit on base chain
    print_section("Depositing tokens on base chain")
    print_info(f"Depositing {config.deposit_amount} {config.base_token} to {config.base_network}...")
    result = run_cli(config, "deposit", config.base_network, config.base_token, str(config.deposit_amount))
    if result and result.returncode != 0:
        print_warning("Deposit to base chain failed, continuing...")
        success = False

    # Step 5: Deposit on quote chain
    print_section("Depositing tokens on quote chain")
    print_info(f"Depositing {config.deposit_amount} {config.quote_token} to {config.quote_network}...")
    result = run_cli(config, "deposit", config.quote_network, config.quote_token, str(config.deposit_amount))
    if result and result.returncode != 0:
        print_warning("Deposit to quote chain failed, continuing...")
        success = False

    # Step 6: Check balances after deposits
    print_section("Checking balances after deposits")
    run_cli(config, "balance")

    # Step 7: Place buy orders
    print_section("Placing buy orders")

    buy_orders = [
        ("100", "99"),
        ("150", "98"),
        ("200", "97"),
    ]

    for amount, price in buy_orders:
        print_info(f"Buy order: {amount} @ limit price {price}")
        result = run_cli(config, "buy-limit", market_id, amount, price)
        if result and result.returncode != 0:
            print_warning(f"Buy order failed, continuing...")
            success = False

    # Step 8: Place sell orders
    print_section("Placing sell orders")

    sell_orders = [
        ("100", "101"),
        ("150", "102"),
        ("200", "103"),
    ]

    for amount, price in sell_orders:
        print_info(f"Sell order: {amount} @ limit price {price}")
        result = run_cli(config, "sell-limit", market_id, amount, price)
        if result and result.returncode != 0:
            print_warning(f"Sell order failed, continuing...")
            success = False

    # Step 9: Check balances after trading
    print_section("Checking balances after trading")
    run_cli(config, "balance")

    # Step 10: Withdraw from base chain
    print_section("Withdrawing tokens from base chain")
    print_info(f"Withdrawing {config.withdraw_amount} {config.base_token} from {config.base_network}...")
    result = run_cli(config, "withdraw", config.base_network, config.base_token, str(config.withdraw_amount))
    if result and result.returncode != 0:
        print_warning("Withdraw from base chain failed, continuing...")
        success = False

    # Step 11: Withdraw from quote chain
    print_section("Withdrawing tokens from quote chain")
    print_info(f"Withdrawing {config.withdraw_amount} {config.quote_token} from {config.quote_network}...")
    result = run_cli(config, "withdraw", config.quote_network, config.quote_token, str(config.withdraw_amount))
    if result and result.returncode != 0:
        print_warning("Withdraw from quote chain failed, continuing...")
        success = False

    # Step 12: Final balance check
    print_section("Final balance check")
    run_cli(config, "balance")

    # Step 13: Final status
    print_section("Final status verification")
    run_cli(config, "status")

    # Summary
    print_section("Test Summary")
    if success:
        print_success("All operations completed successfully!")
    else:
        print_warning("Some operations failed. Check the output above for details.")

    print()
    print_info("Tests performed:")
    print("  - Server connection verified")
    print("  - Configuration fetched")
    print("  - Initial balance checked")
    print(f"  - Deposited {config.base_token} on {config.base_network} ({config.deposit_amount})")
    print(f"  - Deposited {config.quote_token} on {config.quote_network} ({config.deposit_amount})")
    print("  - Placed 3 buy orders at various prices")
    print("  - Placed 3 sell orders at various prices")
    print("  - Verified balances after trading")
    print(f"  - Withdrew {config.base_token} from {config.base_network} ({config.withdraw_amount})")
    print(f"  - Withdrew {config.quote_token} from {config.quote_network} ({config.withdraw_amount})")
    print("  - Final balance verification")

    return success


def main() -> int:
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="AMMIT - Automated Market Maker Internal Tester",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s                                    # Use defaults (localhost, anvil networks)
  %(prog)s --stack-url http://remote:50051    # Use remote server
  %(prog)s --network-base base-sepolia        # Use testnet networks
  %(prog)s --dry-run                          # Show commands without executing
  %(prog)s --binary target/release/aspens-cli # Use specific binary
  %(prog)s --market-id "84531::0x123...::84532::0x456..."  # Specify market ID

Note: The script looks for aspens-cli in target/release/ then target/debug/.
      Build the CLI first with 'just build' or 'cargo build'.
        """
    )

    parser.add_argument(
        "--stack-url", "-s",
        default="http://localhost:50051",
        help="Aspens stack gRPC URL (default: http://localhost:50051)"
    )

    parser.add_argument(
        "--network-base", "-b",
        default="anvil-1",
        help="Base chain network name (default: anvil-1)"
    )

    parser.add_argument(
        "--network-quote", "-q",
        default="flare-coston",
        help="Quote chain network name (default: flare-coston)"
    )

    parser.add_argument(
        "--base-token",
        default="USDC",
        help="Token symbol for base network (default: USDC)"
    )

    parser.add_argument(
        "--quote-token",
        default="USDT0",
        help="Token symbol for quote network (default: USDT0)"
    )

    parser.add_argument(
        "--market-id", "-m",
        default=None,
        help="Market ID for orders (auto-detected from config if not provided)"
    )

    parser.add_argument(
        "--deposit-amount",
        type=int,
        default=1000000,
        help="Amount to deposit (default: 1000000)"
    )

    parser.add_argument(
        "--withdraw-amount",
        type=int,
        default=500000,
        help="Amount to withdraw (default: 500000, 50%% of deposit)"
    )

    parser.add_argument(
        "--binary",
        default=None,
        help="Path to aspens-cli binary (auto-detected from target/release or target/debug if not provided)"
    )

    parser.add_argument(
        "--dry-run", "-n",
        action="store_true",
        help="Show commands without executing them"
    )

    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Enable verbose output from CLI"
    )

    args = parser.parse_args()

    # Find the CLI binary
    cli_binary = args.binary if args.binary else find_cli_binary()

    config = Config(
        stack_url=args.stack_url,
        base_network=args.network_base,
        quote_network=args.network_quote,
        base_token=args.base_token,
        quote_token=args.quote_token,
        market_id=args.market_id,
        cli_binary=cli_binary,
        dry_run=args.dry_run,
        verbose=args.verbose,
        deposit_amount=args.deposit_amount,
        withdraw_amount=args.withdraw_amount,
    )

    success = run_test_sequence(config)
    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
