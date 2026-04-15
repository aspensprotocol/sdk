// SPDX-License-Identifier: BUSL-1.1
pragma solidity 0.8.31;

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
    struct OrderData {
        address outputToken;
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
