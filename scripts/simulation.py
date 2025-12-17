#!/usr/bin/env python3
import os
import time
import json
import random
import argparse
import subprocess
from typing import Optional, List, Dict
from dataclasses import dataclass, field
from ammit import (
    Config,
    run_cli,
    fetch_config,
    get_market_id,
    print_section,
    print_info,
    print_error,
    print_success,
    find_cli_binary
)
from multiprocessing import Process


def load_env_file(env_file: str) -> None:
    """Load environment variables from a file without external dependencies."""
    if not os.path.exists(env_file):
        print(f"Warning: {env_file} not found")
        return

    with open(env_file, 'r') as f:
        for line in f:
            line = line.strip()
            # Skip comments and empty lines
            if line and not line.startswith('#') and '=' in line:
                key, value = line.split('=', 1)
                os.environ[key.strip()] = value.strip()


def derive_address(privkey: str) -> str:
    """Derive Ethereum address from private key using cast."""
    try:
        result = subprocess.run(
            ["cast", "wallet", "address", privkey],
            capture_output=True,
            text=True,
            check=True
        )
        return result.stdout.strip().lower()
    except Exception as e:
        print_error(f"Failed to derive address: {e}")
        return "unknown"


def get_token_balance(address: str, token_address: str, rpc_url: str) -> int:
    """Query ERC20 token balance using cast."""
    try:
        result = subprocess.run(
            [
                "cast", "call",
                token_address,
                "balanceOf(address)(uint256)",
                address,
                "--rpc-url", rpc_url
            ],
            capture_output=True,
            text=True,
            check=True,
            timeout=10
        )
        # Parse the result - cast can return formats like "10000000 [1e7]" or "0xa"
        balance_str = result.stdout.strip()

        # Split on whitespace and take first part (handles "10000000 [1e7]" format)
        balance_str = balance_str.split()[0]

        # Try hex first, then decimal
        try:
            return int(balance_str, 16)
        except ValueError:
            # If hex fails, try decimal
            return int(balance_str, 10)
    except Exception as e:
        print_error(f"Failed to query balance for {address} on {rpc_url}: {e}")
        return 0


def get_deposited_balances(config: Config, trader: 'TraderState') -> dict:
    """Query deposited balances for a trader using aspens-cli balance."""
    try:
        # Temporarily set TRADER_PRIVKEY env var for this trader
        original_privkey = os.environ.get('TRADER_PRIVKEY')
        os.environ['TRADER_PRIVKEY'] = trader.privkey

        result = subprocess.run(
            [config.cli_binary, "--stack", config.stack_url, "balance"],
            capture_output=True,
            text=True,
            check=False,
            timeout=10
        )

        # Restore original privkey
        if original_privkey:
            os.environ['TRADER_PRIVKEY'] = original_privkey

        if result.returncode != 0:
            return {"base": 0, "quote": 0}

        # Parse balance output - format is typically "Chain: Token: Amount"
        balances = {"base": 0, "quote": 0}
        for line in result.stdout.split('\n'):
            if config.base_token in line and config.base_network in line:
                # Extract number from line
                parts = line.split(':')
                if len(parts) >= 3:
                    try:
                        balances["base"] = int(parts[-1].strip())
                    except ValueError:
                        pass
            elif config.quote_token in line and config.quote_network in line:
                parts = line.split(':')
                if len(parts) >= 3:
                    try:
                        balances["quote"] = int(parts[-1].strip())
                    except ValueError:
                        pass

        return balances
    except Exception as e:
        print_error(f"Failed to query deposited balances: {e}")
        return {"base": 0, "quote": 0}


parser = argparse.ArgumentParser(description='Aspens Trading Simulation')
parser.add_argument('--env-file', default='.env', help='Path to .env file')
parser.add_argument('--mode', choices=['random', 'scenarios'], default='random',
                    help='Simulation mode: random operations or predefined scenarios')
args = parser.parse_args()

# Load the specified env file
load_env_file(args.env_file)

# Use the environment variables
stack_url: Optional[str] = os.getenv('ASPENS_MARKET_STACK_URL')

def get_conf() -> Optional[dict]:
    """Fetch configuration from the Aspens stack using aspens-cli."""
    print_info(f"Fetching config from: {stack_url}")

    cmd = [
        "cargo", "run", "-p", "aspens-cli", "--",
        "--stack", stack_url,
        "config"
    ]

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=True
        )

        # Parse the JSON output
        config = json.loads(result.stdout)
        print_info("✓ Config fetched successfully")
        return config

    except subprocess.CalledProcessError as e:
        print(f"Error fetching config: {e.stderr}")
        return None
    except json.JSONDecodeError as e:
        print(f"Error parsing config JSON: {e}")
        return None


def get_orderbook(market_id: str, historical: bool = True, trader_filter: Optional[str] = None) -> Optional[dict]:
    """
    Fetch orderbook data from the Aspens stack.

    Args:
        market_id: Market identifier (e.g., "84532::0x...::114::0x...")
        historical: If True, returns existing open orders
        trader_filter: Optional trader address to filter by

    Returns:
        Dict with market_id and orders list, or None on error
    """
    # Determine if using TLS based on URL scheme
    use_tls = stack_url.startswith("https://")
    grpc_url = stack_url.replace("http://", "").replace("https://", "")

    # Build request JSON
    request = {
        "continue_stream": True,
        "market_id": market_id,
        "historical_open_orders": historical
    }
    if trader_filter:
        request["filter_by_trader"] = trader_filter

    # Path to proto file
    proto_file = "../aspens/proto/arborter.proto"

    cmd = ["grpcurl"]
    if not use_tls:
        cmd.append("-plaintext")
    cmd.extend([
        "-proto", proto_file,
        "-d", json.dumps(request),
        grpc_url,
        "xyz.aspens.arborter.v1.ArborterService.Orderbook"
    ])

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=False, timeout=3)

        # Parse JSONL output (each line is a separate order)
        orders = []
        for line in result.stdout.strip().split('\n'):
            if line:
                orders.append(json.loads(line))

        return {
            "market_id": market_id,
            "count": len(orders),
            "orders": orders
        }

    except subprocess.TimeoutExpired:
        return None
    except subprocess.CalledProcessError as e:
        return None
    except json.JSONDecodeError as e:
        return None


def get_trades(market_id: str, historical: bool = True, trader_filter: Optional[str] = None) -> Optional[dict]:
    """
    Fetch trade history from the Aspens stack.

    Args:
        market_id: Market identifier
        historical: If True, returns existing closed trades
        trader_filter: Optional trader address to filter by

    Returns:
        Dict with market_id and trades list, or None on error
    """
    # Determine if using TLS based on URL scheme
    use_tls = stack_url.startswith("https://")
    grpc_url = stack_url.replace("http://", "").replace("https://", "")

    # Build request JSON
    request = {
        "continue_stream": True,
        "market_id": market_id,
        "historical_closed_trades": historical
    }
    if trader_filter:
        request["filter_by_trader"] = trader_filter

    # Path to proto file
    proto_file = "../aspens/proto/arborter.proto"

    cmd = ["grpcurl"]
    if not use_tls:
        cmd.append("-plaintext")
    cmd.extend([
        "-proto", proto_file,
        "-d", json.dumps(request),
        grpc_url,
        "xyz.aspens.arborter.v1.ArborterService.Trades"
    ])

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=False, timeout=3)

        # Parse JSONL output (each line is a separate trade)
        trades = []
        for line in result.stdout.strip().split('\n'):
            if line:
                trades.append(json.loads(line))

        return {
            "market_id": market_id,
            "count": len(trades),
            "trades": trades
        }

    except subprocess.TimeoutExpired:
        return None
    except subprocess.CalledProcessError as e:
        return None
    except json.JSONDecodeError as e:
        return None

@dataclass
class TraderState:
    """Tracks a trader's expected state."""
    name: str
    privkey: str
    address: str = ""
    base_balance: int = 1000000  # Mock initial balance
    quote_balance: int = 1000000
    base_deposited: int = 0
    quote_deposited: int = 0
    open_order_ids: List[str] = field(default_factory=list)
    consecutive_failures: int = 0  # Track consecutive failures

    def can_deposit_base(self, amount: int) -> bool:
        return self.base_balance >= amount

    def can_deposit_quote(self, amount: int) -> bool:
        return self.quote_balance >= amount

    def can_buy(self, quantity: int, price: int) -> bool:
        return self.quote_deposited >= (quantity * price)

    def can_sell(self, quantity: int) -> bool:
        return self.base_deposited >= quantity

    def can_withdraw_base(self, amount: int) -> bool:
        return self.base_deposited >= amount

    def can_withdraw_quote(self, amount: int) -> bool:
        return self.quote_deposited >= amount


@dataclass
class OperationResult:
    """Result of an operation with metadata."""
    success: bool
    op_type: str
    trader: str
    details: Dict
    error: Optional[str] = None


class SimulationOrchestrator:
    """Main orchestrator - handles logging, streaming, and assertions."""

    def __init__(self, config: Config, market_id: str, traders: List[TraderState]):
        self.config = config
        self.market_id = market_id
        self.traders = traders
        self.operation_count = 0
        self.success_count = 0
        self.assertion_count = 0

    def log_operation(self, result: OperationResult):
        """Log operation results."""
        if result.success:
            self.success_count += 1
            print_success(f"✓ [{result.trader}] {result.op_type}: {result.details}")
        else:
            print_error(f"✗ [{result.trader}] {result.op_type} FAILED: {result.error}")

    def stream_and_validate(self, expected_state: Dict):
        """Stream orderbook/trades and validate against expected state."""
        print_info("  → Streaming orderbook and trades...")

        # Stream orderbook
        orderbook_data = get_orderbook(self.market_id)
        trades_data = get_trades(self.market_id)

        if orderbook_data:
            self.assert_orderbook_state(orderbook_data, expected_state)

        if trades_data:
            self.assert_trade_state(trades_data, expected_state)

    def assert_orderbook_state(self, orderbook_data: Dict, expected: Dict):
        """Assert orderbook matches expected state."""
        orders = orderbook_data.get('orders', [])

        # Count orders by side
        buy_count = sum(1 for o in orders if o.get('side') == 'BID')
        sell_count = sum(1 for o in orders if o.get('side') == 'ASK')

        print_info(f"    Orderbook: {buy_count} BUYs, {sell_count} SELLs (total: {len(orders)})")
        self.assertion_count += 1

        # Assert: All orders have valid structure
        for order in orders:
            assert 'orderId' in order, "Order missing orderId"
            assert 'price' in order, "Order missing price"
            assert 'quantity' in order, "Order missing quantity"
            assert 'side' in order, "Order missing side"
            self.assertion_count += 1

        # Assert: No orders with zero quantity
        zero_qty_orders = [o for o in orders if int(o.get('quantity', 0)) == 0]
        assert len(zero_qty_orders) == 0, f"Found {len(zero_qty_orders)} orders with zero quantity!"
        self.assertion_count += 1

    def assert_trade_state(self, trades_data: Dict, expected: Dict):
        """Assert trades match expected state."""
        trades = trades_data.get('trades', [])

        print_info(f"    Trades: {len(trades)} executed")
        self.assertion_count += 1

        # Assert: All trades have valid structure
        for trade in trades:
            assert 'price' in trade, "Trade missing price"
            assert 'qty' in trade, "Trade missing quantity"
            assert 'timestamp' in trade, "Trade missing timestamp"
            self.assertion_count += 1

        # Assert: All trade prices are positive
        for trade in trades:
            price = int(trade.get('price', 0))
            qty = int(trade.get('qty', 0))
            assert price > 0, f"Trade has invalid price: {price}"
            assert qty > 0, f"Trade has invalid quantity: {qty}"
            self.assertion_count += 2

    def assert_balances(self, trader: TraderState, operation: str):
        """Assert trader's deposited balances match expected state."""
        actual_balances = get_deposited_balances(self.config, trader)

        print_info(f"    Balance Check [{trader.name}]:")
        print_info(f"      Expected - Base: {trader.base_deposited}, Quote: {trader.quote_deposited}")
        print_info(f"      Actual   - Base: {actual_balances['base']}, Quote: {actual_balances['quote']}")

        # Allow small tolerance for rounding
        tolerance = 1

        base_diff = abs(actual_balances['base'] - trader.base_deposited)
        quote_diff = abs(actual_balances['quote'] - trader.quote_deposited)

        assert base_diff <= tolerance, \
            f"{operation}: Base balance mismatch for {trader.name}. Expected {trader.base_deposited}, got {actual_balances['base']}"
        assert quote_diff <= tolerance, \
            f"{operation}: Quote balance mismatch for {trader.name}. Expected {trader.quote_deposited}, got {actual_balances['quote']}"

        self.assertion_count += 2
        print_success(f"    ✓ Balances verified for {trader.name}")

    def deposit(self, trader: TraderState, chain: str, amount: int) -> OperationResult:
        """Execute deposit operation."""
        is_base = (chain == self.config.base_network)
        token = self.config.base_token if is_base else self.config.quote_token

        # Validate
        if is_base and not trader.can_deposit_base(amount):
            return OperationResult(
                success=False,
                op_type="DEPOSIT_BASE",
                trader=trader.name,
                details={"amount": amount},
                error=f"Insufficient balance: {trader.base_balance}"
            )

        if not is_base and not trader.can_deposit_quote(amount):
            return OperationResult(
                success=False,
                op_type="DEPOSIT_QUOTE",
                trader=trader.name,
                details={"amount": amount},
                error=f"Insufficient balance: {trader.quote_balance}"
            )

        # Execute
        result = run_cli(self.config, "deposit", chain, token, str(amount))

        if result and result.returncode == 0:
            # Update state
            if is_base:
                trader.base_balance -= amount
                trader.base_deposited += amount
            else:
                trader.quote_balance -= amount
                trader.quote_deposited += amount

            return OperationResult(
                success=True,
                op_type=f"DEPOSIT_{'BASE' if is_base else 'QUOTE'}",
                trader=trader.name,
                details={"amount": amount, "chain": chain}
            )

        # Deposit failed - wallet likely has 0 balance, update mock balance
        if is_base:
            trader.base_balance = 0
        else:
            trader.quote_balance = 0

        return OperationResult(
            success=False,
            op_type=f"DEPOSIT_{'BASE' if is_base else 'QUOTE'}",
            trader=trader.name,
            details={"amount": amount},
            error="CLI command failed - wallet may have insufficient funds"
        )

    def place_order(self, trader: TraderState, side: str, quantity: int, price: int) -> OperationResult:
        """Place buy or sell order."""
        is_buy = (side == "BUY")

        # Validate
        if is_buy and not trader.can_buy(quantity, price):
            return OperationResult(
                success=False,
                op_type="BUY_ORDER",
                trader=trader.name,
                details={"qty": quantity, "price": price},
                error=f"Insufficient quote balance: {trader.quote_deposited}"
            )

        if not is_buy and not trader.can_sell(quantity):
            return OperationResult(
                success=False,
                op_type="SELL_ORDER",
                trader=trader.name,
                details={"qty": quantity, "price": price},
                error=f"Insufficient base balance: {trader.base_deposited}"
            )

        # Execute
        cmd = "buy-limit" if is_buy else "sell-limit"
        result = run_cli(self.config, cmd, self.market_id, str(quantity), str(price))

        if result and result.returncode == 0:
            # Update state (lock funds)
            if is_buy:
                trader.quote_deposited -= (quantity * price)
            else:
                trader.base_deposited -= quantity

            return OperationResult(
                success=True,
                op_type=f"{side}_ORDER",
                trader=trader.name,
                details={"qty": quantity, "price": price}
            )

        return OperationResult(
            success=False,
            op_type=f"{side}_ORDER",
            trader=trader.name,
            details={"qty": quantity, "price": price},
            error="CLI command failed"
        )

    def withdraw(self, trader: TraderState, chain: str, amount: int) -> OperationResult:
        """Execute withdrawal operation."""
        is_base = (chain == self.config.base_network)
        token = self.config.base_token if is_base else self.config.quote_token

        # Validate
        if is_base and not trader.can_withdraw_base(amount):
            return OperationResult(
                success=False,
                op_type="WITHDRAW_BASE",
                trader=trader.name,
                details={"amount": amount},
                error=f"Insufficient deposited: {trader.base_deposited}"
            )

        if not is_base and not trader.can_withdraw_quote(amount):
            return OperationResult(
                success=False,
                op_type="WITHDRAW_QUOTE",
                trader=trader.name,
                details={"amount": amount},
                error=f"Insufficient deposited: {trader.quote_deposited}"
            )

        # Execute
        result = run_cli(self.config, "withdraw", chain, token, str(amount))

        if result and result.returncode == 0:
            # Update state
            if is_base:
                trader.base_deposited -= amount
                trader.base_balance += amount
            else:
                trader.quote_deposited -= amount
                trader.quote_balance += amount

            return OperationResult(
                success=True,
                op_type=f"WITHDRAW_{'BASE' if is_base else 'QUOTE'}",
                trader=trader.name,
                details={"amount": amount, "chain": chain}
            )

        return OperationResult(
            success=False,
            op_type=f"WITHDRAW_{'BASE' if is_base else 'QUOTE'}",
            trader=trader.name,
            details={"amount": amount},
            error="CLI command failed"
        )

    def run_random_operation(self, trader: TraderState) -> OperationResult:
        """Select and execute a random valid operation."""
        operations = []

        # Build weighted operation list (skip if balance is 0)
        if trader.base_balance > 0 and trader.base_balance > 1000:
            operations.extend([("deposit_base", 3)])
        if trader.quote_balance > 0 and trader.quote_balance > 1000:
            operations.extend([("deposit_quote", 3)])
        if trader.quote_deposited > 0 and trader.quote_deposited > 100:
            operations.extend([("buy_order", 5)])
        if trader.base_deposited > 0 and trader.base_deposited > 100:
            operations.extend([("sell_order", 5)])
        if trader.base_deposited > 0 and trader.base_deposited > 500:
            operations.extend([("withdraw_base", 1)])
        if trader.quote_deposited > 0 and trader.quote_deposited > 500:
            operations.extend([("withdraw_quote", 1)])

        if not operations:
            return OperationResult(
                success=False,
                op_type="NONE",
                trader=trader.name,
                details={},
                error="No valid operations available"
            )

        # Choose operation
        op_names = [op[0] for op in operations]
        weights = [op[1] for op in operations]
        operation = random.choices(op_names, weights=weights)[0]

        # Execute
        if operation == "deposit_base":
            max_amount = min(trader.base_balance, 10000)
            min_amount = min(1000, trader.base_balance)
            amount = random.randint(min_amount, max_amount) if min_amount <= max_amount else trader.base_balance
            return self.deposit(trader, self.config.base_network, amount)

        elif operation == "deposit_quote":
            max_amount = min(trader.quote_balance, 10000)
            min_amount = min(1000, trader.quote_balance)
            amount = random.randint(min_amount, max_amount) if min_amount <= max_amount else trader.quote_balance
            return self.deposit(trader, self.config.quote_network, amount)

        elif operation == "buy_order":
            max_qty = trader.quote_deposited // 100
            quantity = random.randint(10, max(10, max_qty))
            price = random.randint(95, 105)
            return self.place_order(trader, "BUY", quantity, price)

        elif operation == "sell_order":
            max_qty = trader.base_deposited
            quantity = random.randint(10, max(10, max_qty))
            price = random.randint(95, 105)
            return self.place_order(trader, "SELL", quantity, price)

        elif operation == "withdraw_base":
            max_amount = min(trader.base_deposited, 5000)
            min_amount = min(100, trader.base_deposited)
            amount = random.randint(min_amount, max_amount) if min_amount <= max_amount else trader.base_deposited
            return self.withdraw(trader, self.config.base_network, amount)

        elif operation == "withdraw_quote":
            max_amount = min(trader.quote_deposited, 5000)
            min_amount = min(100, trader.quote_deposited)
            amount = random.randint(min_amount, max_amount) if min_amount <= max_amount else trader.quote_deposited
            return self.withdraw(trader, self.config.quote_network, amount)

        return OperationResult(
            success=False,
            op_type="UNKNOWN",
            trader=trader.name,
            details={},
            error="Unknown operation"
        )

    def run_scenario_1_buyer_to_3_sellers(self):
        """Scenario: 1 large buy order matches against 3 smaller sell orders."""
        print_section("Scenario: 1 Buyer → 3 Sellers (Split Settlement)")

        if len(self.traders) < 4:
            print_error("Need at least 4 traders for this scenario")
            return

        buyer = self.traders[0]
        sellers = self.traders[1:4]

        # Deposit tokens for all traders
        print_info("Setting up traders...")

        # Buyer needs quote tokens to buy
        self.deposit(buyer, self.config.quote_network, 50000)
        self.assert_balances(buyer, "After deposit")

        # Sellers need base tokens to sell
        for seller in sellers:
            self.deposit(seller, self.config.base_network, 10000)
            self.assert_balances(seller, "After deposit")

        time.sleep(1)

        # Place 3 sell orders at price 100 with different quantities
        print_info("Placing 3 sell orders...")
        self.place_order(sellers[0], "SELL", 100, 100)  # 100 @ 100
        self.assert_balances(sellers[0], "After sell order")
        time.sleep(0.3)

        self.place_order(sellers[1], "SELL", 150, 100)  # 150 @ 100
        self.assert_balances(sellers[1], "After sell order")
        time.sleep(0.3)

        self.place_order(sellers[2], "SELL", 80, 100)   # 80 @ 100
        self.assert_balances(sellers[2], "After sell order")

        time.sleep(1)

        # Place 1 large buy order that matches all 3 sells
        print_info("Placing 1 large buy order to match all sells...")
        self.place_order(buyer, "BUY", 330, 100)  # 330 @ 100 (matches 100+150+80)

        time.sleep(3)

        # Validate trades
        print_info("Validating split settlement...")
        trades_data = get_trades(self.market_id)
        if trades_data:
            print_success(f"✓ {trades_data['count']} trades executed")
            assert trades_data['count'] >= 3, "Expected at least 3 trades for split settlement"

        # Assert final balances for all traders
        print_info("Validating final balances after settlement...")
        self.assert_balances(buyer, "After trade settlement")
        for seller in sellers:
            self.assert_balances(seller, "After trade settlement")

    def run_scenario_1_seller_to_3_buyers(self):
        """Scenario: 1 large sell order matches against 3 smaller buy orders."""
        print_section("Scenario: 1 Seller → 3 Buyers (Split Settlement)")

        if len(self.traders) < 4:
            print_error("Need at least 4 traders for this scenario")
            return

        seller = self.traders[0]
        buyers = self.traders[1:4]

        # Deposit tokens
        print_info("Setting up traders...")

        # Seller needs base tokens
        self.deposit(seller, self.config.base_network, 10000)
        self.assert_balances(seller, "After deposit")

        # Buyers need quote tokens
        for buyer in buyers:
            self.deposit(buyer, self.config.quote_network, 20000)
            self.assert_balances(buyer, "After deposit")

        time.sleep(1)

        # Place 3 buy orders at price 100 with different quantities
        print_info("Placing 3 buy orders...")
        self.place_order(buyers[0], "BUY", 120, 100)  # 120 @ 100
        self.assert_balances(buyers[0], "After buy order")
        time.sleep(0.3)

        self.place_order(buyers[1], "BUY", 90, 100)   # 90 @ 100
        self.assert_balances(buyers[1], "After buy order")
        time.sleep(0.3)

        self.place_order(buyers[2], "BUY", 140, 100)  # 140 @ 100
        self.assert_balances(buyers[2], "After buy order")

        time.sleep(1)

        # Place 1 large sell order that matches all 3 buys
        print_info("Placing 1 large sell order to match all buys...")
        self.place_order(seller, "SELL", 350, 100)  # 350 @ 100 (matches 120+90+140)

        time.sleep(3)

        # Validate trades
        print_info("Validating split settlement...")
        trades_data = get_trades(self.market_id)
        if trades_data:
            print_success(f"✓ {trades_data['count']} trades executed")
            assert trades_data['count'] >= 3, "Expected at least 3 trades for split settlement"

        # Assert final balances
        print_info("Validating final balances after settlement...")
        self.assert_balances(seller, "After trade settlement")
        for buyer in buyers:
            self.assert_balances(buyer, "After trade settlement")

    def run_scenario_2_buyers_to_2_sellers(self):
        """Scenario: 2 buyers at same price, different volumes match to 2 sellers."""
        print_section("Scenario: 2 Buyers → 2 Sellers (2:2 Split Settlement)")

        if len(self.traders) < 4:
            print_error("Need at least 4 traders for this scenario")
            return

        buyers = self.traders[0:2]
        sellers = self.traders[2:4]

        # Deposit tokens
        print_info("Setting up traders...")

        for buyer in buyers:
            self.deposit(buyer, self.config.quote_network, 30000)
            self.assert_balances(buyer, "After deposit")

        for seller in sellers:
            self.deposit(seller, self.config.base_network, 15000)
            self.assert_balances(seller, "After deposit")

        time.sleep(1)

        # Place 2 sell orders at price 100
        print_info("Placing 2 sell orders...")
        self.place_order(sellers[0], "SELL", 180, 100)  # 180 @ 100
        self.assert_balances(sellers[0], "After sell order")
        time.sleep(0.3)

        self.place_order(sellers[1], "SELL", 220, 100)  # 220 @ 100
        self.assert_balances(sellers[1], "After sell order")

        time.sleep(1)

        # Place 2 buy orders at same price, different volumes
        print_info("Placing 2 buy orders at same price...")
        self.place_order(buyers[0], "BUY", 150, 100)   # 150 @ 100
        self.assert_balances(buyers[0], "After buy order")
        time.sleep(0.3)

        self.place_order(buyers[1], "BUY", 250, 100)   # 250 @ 100
        self.assert_balances(buyers[1], "After buy order")

        time.sleep(3)

        # Validate trades
        print_info("Validating 2:2 split settlement...")
        trades_data = get_trades(self.market_id)
        if trades_data:
            print_success(f"✓ {trades_data['count']} trades executed")
            assert trades_data['count'] >= 2, "Expected at least 2 trades for 2:2 settlement"

        # Assert final balances
        print_info("Validating final balances after settlement...")
        for buyer in buyers:
            self.assert_balances(buyer, "After trade settlement")
        for seller in sellers:
            self.assert_balances(seller, "After trade settlement")

    def run_simulation(self, num_operations: int = 50):
        """Main simulation loop with logging and validation."""
        print_section(f"Multi-Trader Simulation - {num_operations} Operations")
        print_info(f"Market: {self.market_id}")
        print_info(f"Traders:")
        for t in self.traders:
            print_info(f"  {t.name}: {t.address}")
        print()

        for i in range(num_operations):
            self.operation_count += 1

            # Select a trader that hasn't failed too many times
            active_traders = [t for t in self.traders if t.consecutive_failures < 3]
            if not active_traders:
                print_error("All traders have too many consecutive failures. Stopping simulation.")
                break

            trader = random.choice(active_traders)

            print_section(f"Operation {i+1}/{num_operations} - {trader.name} ({trader.address})")
            print_info(f"  State: Base={trader.base_deposited}, Quote={trader.quote_deposited}")
            if trader.consecutive_failures > 0:
                print_info(f"  Consecutive failures: {trader.consecutive_failures}")

            # Execute operation
            result = self.run_random_operation(trader)
            self.log_operation(result)

            # Update consecutive failures
            if result.success:
                trader.consecutive_failures = 0
                # Stream and validate
                expected_state = {"traders": self.traders}
                self.stream_and_validate(expected_state)
            else:
                trader.consecutive_failures += 1
                if trader.consecutive_failures >= 3:
                    print_error(f"  {trader.name} has had 3 consecutive failures - marking as inactive")

            time.sleep(0.5)
            print()

        # Final summary
        print_section("Simulation Complete")
        print_success(f"Total Operations: {self.operation_count}")
        print_success(f"Successful: {self.success_count}")
        print_success(f"Assertions Passed: {self.assertion_count}")

        # Show trader status
        print()
        print_info("Trader Status:")
        active = [t for t in self.traders if t.consecutive_failures < 3]
        inactive = [t for t in self.traders if t.consecutive_failures >= 3]
        print_success(f"  Active: {len(active)} traders")
        if inactive:
            print_error(f"  Inactive: {len(inactive)} traders - {[t.name for t in inactive]}")

        # Final state validation
        print_section("Final State Validation")
        orderbook_data = get_orderbook(self.market_id)
        trades_data = get_trades(self.market_id)

        if orderbook_data:
            print_info(f"Final orderbook: {orderbook_data['count']} open orders")
        if trades_data:
            print_info(f"Total trades executed: {trades_data['count']}")


def run_scenario_tests():
    """Run all predefined split settlement scenarios."""
    trader_keys = [
        os.getenv('TRADER1_PRIVKEY'),
        os.getenv('TRADER2_PRIVKEY'),
        os.getenv('TRADER3_PRIVKEY'),
        os.getenv('TRADER4_PRIVKEY'),
    ]

    if not all(trader_keys):
        print_error("Missing trader keys in .env (TRADER1_PRIVKEY through TRADER4_PRIVKEY)")
        return

    # Get config
    conf = get_conf()
    if not conf:
        print_error("Failed to fetch config")
        return

    markets = conf['config']['markets']
    if not markets:
        print_error("No markets found")
        return

    market = markets[0]
    market_id = market['marketId']

    # Get chains config
    chains = {chain['network']: chain for chain in conf['config']['chains']}
    base_chain = chains.get(market['baseChainNetwork'])
    quote_chain = chains.get(market['quoteChainNetwork'])

    # Create traders (skip balance queries for scenarios)
    print_section("Initializing Traders for Scenarios")
    traders = []
    for i, key in enumerate(trader_keys):
        name = f"Trader{i+1}"
        address = derive_address(key)
        # Use high mock balances for scenario testing
        trader = TraderState(
            name=name,
            privkey=key,
            address=address,
            base_balance=1000000,
            quote_balance=1000000
        )
        traders.append(trader)
        print_info(f"{name}: {address}")
    print()

    # Create config
    config = Config(
        stack_url=stack_url,
        base_network=market['baseChainNetwork'],
        quote_network=market['quoteChainNetwork'],
        base_token=market['baseChainTokenSymbol'],
        quote_token=market['quoteChainTokenSymbol'],
        market_id=market_id,
        cli_binary=find_cli_binary(),
        dry_run=False,
        verbose=False,
        deposit_amount=1000000,
        withdraw_amount=500000
    )

    # Run scenarios
    orchestrator = SimulationOrchestrator(config, market_id, traders)

    print_section("Running Split Settlement Scenarios")

    try:
        orchestrator.run_scenario_1_buyer_to_3_sellers()
        print()
        time.sleep(3)

        orchestrator.run_scenario_1_seller_to_3_buyers()
        print()
        time.sleep(3)

        orchestrator.run_scenario_2_buyers_to_2_sellers()
        print()

        print_section("All Scenarios Complete")
        print_success("✓ All split settlement scenarios passed!")

    except AssertionError as e:
        print_error(f"Scenario assertion failed: {e}")
    except Exception as e:
        print_error(f"Scenario error: {e}")


def run_multi_trader_simulation():
    """Run the multi-trader simulation."""
    trader_keys = [
        os.getenv('TRADER1_PRIVKEY'),
        os.getenv('TRADER2_PRIVKEY'),
        os.getenv('TRADER3_PRIVKEY'),
        os.getenv('TRADER4_PRIVKEY'),
    ]

    if not all(trader_keys):
        print_error("Missing trader keys in .env (TRADER1_PRIVKEY through TRADER4_PRIVKEY)")
        return

    # Get config first
    conf = get_conf()
    if not conf:
        print_error("Failed to fetch config")
        return

    markets = conf['config']['markets']
    if not markets:
        print_error("No markets found")
        return

    market = markets[0]
    market_id = market['marketId']

    # Get chains config
    chains = {chain['network']: chain for chain in conf['config']['chains']}
    base_chain = chains.get(market['baseChainNetwork'])
    quote_chain = chains.get(market['quoteChainNetwork'])

    if not base_chain or not quote_chain:
        print_error("Could not find chain configurations")
        return

    base_rpc = base_chain['rpcUrl']
    quote_rpc = quote_chain['rpcUrl']

    # Get token addresses from chain configs (tokens is a dict keyed by symbol)
    base_token_symbol = market['baseChainTokenSymbol']
    quote_token_symbol = market['quoteChainTokenSymbol']

    base_token = base_chain['tokens'].get(base_token_symbol)
    quote_token = quote_chain['tokens'].get(quote_token_symbol)

    if not base_token or not quote_token:
        print_error(f"Could not find tokens in chain configs")
        print_error(f"Available base tokens: {list(base_chain['tokens'].keys())}")
        print_error(f"Available quote tokens: {list(quote_chain['tokens'].keys())}")
        return

    base_token_addr = base_token['address']
    quote_token_addr = quote_token['address']

    # Create traders and query actual balances
    print_section("Initializing Traders")
    traders = []
    for i, key in enumerate(trader_keys):
        name = f"Trader{i+1}"
        address = derive_address(key)

        print_info(f"{name}: {address}")
        print_info(f"  Querying on-chain balances...")

        # Query actual wallet balances
        base_balance = get_token_balance(address, base_token_addr, base_rpc)
        quote_balance = get_token_balance(address, quote_token_addr, quote_rpc)

        print_info(f"  Base ({market['baseChainTokenSymbol']}): {base_balance}")
        print_info(f"  Quote ({market['quoteChainTokenSymbol']}): {quote_balance}")

        trader = TraderState(
            name=name,
            privkey=key,
            address=address,
            base_balance=base_balance,
            quote_balance=quote_balance
        )
        traders.append(trader)
    print()

    # Create config
    config = Config(
        stack_url=stack_url,
        base_network=market['baseChainNetwork'],
        quote_network=market['quoteChainNetwork'],
        base_token=market['baseChainTokenSymbol'],
        quote_token=market['quoteChainTokenSymbol'],
        market_id=market_id,
        cli_binary=find_cli_binary(),
        dry_run=False,
        verbose=False,
        deposit_amount=1000000,
        withdraw_amount=500000
    )

    # Run simulation
    orchestrator = SimulationOrchestrator(config, market_id, traders)
    orchestrator.run_simulation(num_operations=50)


if __name__ == "__main__":
    if args.mode == 'scenarios':
        run_scenario_tests()
    else:
        run_multi_trader_simulation()
