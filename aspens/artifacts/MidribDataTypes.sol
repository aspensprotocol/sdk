// SPDX-License-Identifier: BUSL-1.1
pragma solidity 0.8.34;

/**
 * @title Midrib DataTypes
 * @notice Library to manage custom dataypes in the Midrib Contracts
 * @author Aspens technical team
 *
 */
library MidribDataTypes {
    /// @dev Datatype for keeping track of orders
    struct Order {
        uint160 amount;
        bool isCanceled;
        address token;
        address trader;
    }

    //bytes32 crossChainId;

    /// @dev Datatype for keeping track of order data
    /// @dev `outputToken` is `bytes32` (not `address`) so cross-chain orders to
    ///      non-EVM destinations can carry a 32-byte token identifier (e.g. a
    ///      Solana mint pubkey). EVM destinations encode the address
    ///      left-padded to 32 bytes (`bytes32(uint256(uint160(addr)))`).
    ///      Used as commitment data only — it's hashed into the EIP-712
    ///      digest and emitted on-chain, but no token transfer logic
    ///      consults it (the lock token comes from the Permit2 details).
    struct OrderData {
        bytes32 outputToken;
        uint160 outputAmount;
        uint160 inputAmount;
        address recipient;
        uint256 destinationChainId;
        address exclusiveRelayer;
        bytes message;
    }

    /// @dev Datatype for keeping track of filled order data
    struct FilledOrder {
        address token;
        SettleFor settleFor;
        IntentAction action;
        address fromAddress;
        address toAddress;
        uint160 amount;
        uint256 repaymentChainId;
        bytes32 orderId;
    }

    /// @dev Enum for keeping track of who the order is being settled for
    enum SettleFor {
        MAKER,
        TAKER
    }

    /// @dev Enum for keeping track of the intent action
    enum IntentAction {
        DEPOSIT,
        DEPOSIT_AND_LOCK,
        LOCK,
        SETTLE,
        SETTLE_AND_WITHDRAW,
        WITHDRAW,
        CANCEL
    }
}
