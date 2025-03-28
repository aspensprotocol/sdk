// SPDX-License-Identifier: MIT
pragma solidity ^0.8.29;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract TempToken is ERC20 {
    uint8 private immutable _decimals;
    
    constructor() ERC20("TOKEN_NAME", "TOKEN_SYMBOL") {
        _decimals = TOKEN_DECIMALS;
        _mint(msg.sender, 1000000 * 10**_decimals);
    }

    function decimals() public view virtual override returns (uint8) {
        return _decimals;
    }
} 