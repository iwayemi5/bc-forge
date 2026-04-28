//! # bc-forge Token Contract
//!
//! A Soroban-based token contract implementing the standard SEP-41 TokenInterface
//! with additional administrative controls, pausable lifecycle, and ownership management.
//!
//! ## Features
//! - SEP-41 compliant (balance, transfer, approve, burn)
//! - Admin-only minting with supply tracking
//! - Emergency pause/unpause via lifecycle module
//! - Two-step ownership transfer support
//! - Structured event emissions for off-chain indexing

#![no_std]

mod events;

#[cfg(test)]
mod test;

use soroban_sdk::token::TokenInterface;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, String,
};

/// Errors returned by the token contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TokenError {
    /// The contract was initialized more than once.
    AlreadyInitialized = 1,
    /// The contract has not been initialized yet.
    NotInitialized = 2,
    /// The source account does not have enough tokens.
    InsufficientBalance = 3,
    /// The approved allowance is too small for the requested action.
    InsufficientAllowance = 4,
    /// The provided amount is invalid for this operation.
    InvalidAmount = 5,
    /// The contract is currently paused.
    ContractPaused = 6,
}

/// Storage keys for the token contract state.
#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    /// The contract admin address.
    Admin,
    /// Spending allowance: (owner, spender) → amount.
    Allowance(Address, Address),
    /// Token balance for an address.
    Balance(Address),
    /// Token name (human-readable).
    Name,
    /// Token ticker symbol.
    Symbol,
    /// Number of decimal places.
    Decimals,
    /// Total token supply.
    Supply,
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract Definition
// ─────────────────────────────────────────────────────────────────────────────

#[contract]
pub struct BcForgeToken;

// ─────────────────────────────────────────────────────────────────────────────
// Internal Helpers
// ─────────────────────────────────────────────────────────────────────────────

impl BcForgeToken {
    /// Returns an initialized admin address or a contract error.
    fn read_admin(env: &Env) -> Result<Address, TokenError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(TokenError::NotInitialized)
    }

    /// Returns `Ok(())` when the contract has been initialized.
    fn ensure_initialized(env: &Env) -> Result<(), TokenError> {
        if env.storage().instance().has(&DataKey::Admin) {
            Ok(())
        } else {
            Err(TokenError::NotInitialized)
        }
    }

    /// Returns `Ok(())` when the contract is not paused.
    fn ensure_not_paused(env: &Env) -> Result<(), TokenError> {
        if bc_forge_lifecycle::is_paused(env) {
            Err(TokenError::ContractPaused)
        } else {
            Ok(())
        }
    }

    /// Panics with a contract error if the result is `Err`.
    fn panic_on_err<T>(env: &Env, result: Result<T, TokenError>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => soroban_sdk::panic_with_error!(env, error),
        }
    }

    /// Reads the balance for a given address, defaulting to 0.
    fn read_balance(env: &Env, id: &Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Balance(id.clone()))
            .unwrap_or(0)
    }

    /// Writes a balance for a given address.
    fn write_balance(env: &Env, id: &Address, balance: i128) {
        env.storage()
            .persistent()
            .set(&DataKey::Balance(id.clone()), &balance);
    }

    /// Reads the spending allowance for (owner → spender), defaulting to 0.
    fn read_allowance(env: &Env, from: &Address, spender: &Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Allowance(from.clone(), spender.clone()))
            .unwrap_or(0)
    }

    /// Writes a spending allowance for (owner → spender).
    fn write_allowance(env: &Env, from: &Address, spender: &Address, amount: i128) {
        env.storage()
            .persistent()
            .set(&DataKey::Allowance(from.clone(), spender.clone()), &amount);
    }

    /// Moves `amount` tokens from `from` to `to`.
    /// Returns the new balances (from_balance, to_balance).
    fn move_balance(
        env: &Env,
        from: &Address,
        to: &Address,
        amount: i128,
    ) -> Result<(i128, i128), TokenError> {
        let from_balance = Self::read_balance(env, from);
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }

        // Self-transfer is a no-op on balances.
        if from == to {
            return Ok((from_balance, from_balance));
        }

        let new_from = from_balance - amount;
        let new_to = Self::read_balance(env, to) + amount;

        Self::write_balance(env, from, new_from);
        Self::write_balance(env, to, new_to);

        Ok((new_from, new_to))
    }

    /// Reads the total supply, defaulting to 0.
    fn read_supply(env: &Env) -> i128 {
        env.storage().instance().get(&DataKey::Supply).unwrap_or(0)
    }

    /// Writes the total supply.
    fn write_supply(env: &Env, supply: i128) {
        env.storage().instance().set(&DataKey::Supply, &supply);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Custom Admin / Lifecycle Functions
// ─────────────────────────────────────────────────────────────────────────────

#[contractimpl]
impl BcForgeToken {
    /// Initializes the token contract with an admin and metadata.
    ///
    /// # Arguments
    /// * `admin`   - The address that will have minting privileges.
    /// * `decimal` - Number of decimal places (e.g., 7 for Stellar standard).
    /// * `name`    - Human-readable token name.
    /// * `symbol`  - Token ticker symbol.
    pub fn initialize(
        env: Env,
        admin: Address,
        decimal: u32,
        name: String,
        symbol: String,
    ) -> Result<(), TokenError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(TokenError::AlreadyInitialized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Decimals, &decimal);
        env.storage().instance().set(&DataKey::Name, &name);
        env.storage().instance().set(&DataKey::Symbol, &symbol);
        Self::write_supply(&env, 0);

        events::emit_initialized(&env, &admin, decimal, &name, &symbol);

        Ok(())
    }

    /// Mints `amount` tokens to the `to` address. Admin-only.
    ///
    /// # Arguments
    /// * `to`     - Recipient address.
    /// * `amount` - Number of tokens to mint (must be positive).
    pub fn mint(env: Env, to: Address, amount: i128) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        Self::ensure_not_paused(&env)?;

        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }

        let admin = Self::read_admin(&env)?;
        admin.require_auth();

        let balance = Self::read_balance(&env, &to) + amount;
        Self::write_balance(&env, &to, balance);

        let supply = Self::read_supply(&env) + amount;
        Self::write_supply(&env, supply);

        events::emit_mint(&env, &admin, &to, amount, balance, supply);

        Ok(())
    }

    /// Transfers the admin role to a new address. Current admin-only.
    ///
    /// # Arguments
    /// * `new_admin` - The address to receive admin privileges.
    pub fn transfer_ownership(env: Env, new_admin: Address) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        let admin = Self::read_admin(&env)?;
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        events::emit_ownership_transferred(&env, &admin, &new_admin);

        Ok(())
    }

    /// Returns the total token supply.
    pub fn supply(env: Env) -> i128 {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        env.storage().instance().get(&DataKey::Supply).unwrap_or(0)
    }

    /// Pauses all token operations. Admin-only.
    pub fn pause(env: Env) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        let admin = Self::read_admin(&env)?;
        bc_forge_lifecycle::pause(env.clone(), admin.clone());
        events::emit_paused(&env, &admin);

        Ok(())
    }

    /// Unpauses token operations. Admin-only.
    pub fn unpause(env: Env) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        let admin = Self::read_admin(&env)?;
        bc_forge_lifecycle::unpause(env.clone(), admin.clone());
        events::emit_unpaused(&env, &admin);

        Ok(())
    }

    /// Upgrades the contract to a new WASM hash. Admin-only.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        let admin = Self::read_admin(&env)?;
        admin.require_auth();

        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        events::emit_upgrade(&env, &admin, &new_wasm_hash);

        Ok(())
    }

    /// Returns the contract version.
    pub fn version(env: Env) -> String {
        String::from_str(&env, "1.0.0")
    }

    /// Updates the token name. Admin-only.
    pub fn update_name(env: Env, new_name: String) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        let admin = Self::read_admin(&env)?;
        admin.require_auth();

        let old_name = env
            .storage()
            .instance()
            .get(&DataKey::Name)
            .unwrap_or_else(|| String::from_str(&env, "bc-forge"));

        env.storage().instance().set(&DataKey::Name, &new_name);
        events::emit_update_name(&env, &admin, &old_name, &new_name);

        Ok(())
    }

    /// Updates the token symbol. Admin-only.
    pub fn update_symbol(env: Env, new_symbol: String) -> Result<(), TokenError> {
        Self::ensure_initialized(&env)?;
        let admin = Self::read_admin(&env)?;
        admin.require_auth();

        let old_symbol = env
            .storage()
            .instance()
            .get(&DataKey::Symbol)
            .unwrap_or_else(|| String::from_str(&env, "SFG"));

        env.storage().instance().set(&DataKey::Symbol, &new_symbol);
        events::emit_update_symbol(&env, &admin, &old_symbol, &new_symbol);

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SEP-41 TokenInterface Implementation
// ─────────────────────────────────────────────────────────────────────────────

#[contractimpl]
impl TokenInterface for BcForgeToken {
    /// Returns the spending allowance granted by `from` to `spender`.
    fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        Self::read_allowance(&env, &from, &spender)
    }

    /// Approves `spender` to spend up to `amount` tokens on behalf of `from`.
    ///
    /// # Arguments
    /// * `from`    - The token owner granting the allowance.
    /// * `spender` - The address being granted spending rights.
    /// * `amount`  - Maximum tokens the spender can use.
    /// * `_exp`    - Expiration ledger (reserved, currently unused).
    fn approve(env: Env, from: Address, spender: Address, amount: i128, _exp: u32) {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        from.require_auth();
        if amount < 0 {
            soroban_sdk::panic_with_error!(&env, TokenError::InvalidAmount);
        }
        Self::write_allowance(&env, &from, &spender, amount);
        events::emit_approve(&env, &from, &spender, amount);
    }

    /// Returns the token balance for the given address.
    fn balance(env: Env, id: Address) -> i128 {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        Self::read_balance(&env, &id)
    }

    /// Transfers `amount` tokens from `from` to `to`.
    fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        Self::panic_on_err(&env, Self::ensure_not_paused(&env));
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        from.require_auth();

        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, TokenError::InvalidAmount);
        }

        let _ = Self::panic_on_err(&env, Self::move_balance(&env, &from, &to, amount));
        events::emit_transfer(&env, &from, &to, amount);
    }

    /// Transfers `amount` tokens from `from` to `to` using `spender`'s allowance.
    fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        Self::panic_on_err(&env, Self::ensure_not_paused(&env));
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        spender.require_auth();

        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, TokenError::InvalidAmount);
        }

        let allowance = Self::read_allowance(&env, &from, &spender);
        if allowance < amount {
            soroban_sdk::panic_with_error!(&env, TokenError::InsufficientAllowance);
        }

        let _ = Self::panic_on_err(&env, Self::move_balance(&env, &from, &to, amount));
        Self::write_allowance(&env, &from, &spender, allowance - amount);
        events::emit_transfer_from(&env, &spender, &from, &to, amount, allowance - amount);
    }

    /// Burns `amount` tokens from `from`'s balance, reducing total supply.
    fn burn(env: Env, from: Address, amount: i128) {
        Self::panic_on_err(&env, Self::ensure_not_paused(&env));
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        from.require_auth();

        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, TokenError::InvalidAmount);
        }

        let balance = Self::read_balance(&env, &from);
        if balance < amount {
            soroban_sdk::panic_with_error!(&env, TokenError::InsufficientBalance);
        }

        let new_balance = balance - amount;
        Self::write_balance(&env, &from, new_balance);

        let supply = Self::read_supply(&env) - amount;
        Self::write_supply(&env, supply);

        events::emit_burn(&env, &from, amount, new_balance, supply);
    }

    /// Burns `amount` tokens from `from` using `spender`'s allowance.
    fn burn_from(env: Env, spender: Address, from: Address, amount: i128) {
        Self::panic_on_err(&env, Self::ensure_not_paused(&env));
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        spender.require_auth();

        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, TokenError::InvalidAmount);
        }

        let allowance = Self::read_allowance(&env, &from, &spender);
        if allowance < amount {
            soroban_sdk::panic_with_error!(&env, TokenError::InsufficientAllowance);
        }

        let balance = Self::read_balance(&env, &from);
        if balance < amount {
            soroban_sdk::panic_with_error!(&env, TokenError::InsufficientBalance);
        }

        Self::write_allowance(&env, &from, &spender, allowance - amount);
        Self::write_balance(&env, &from, balance - amount);

        let supply = Self::read_supply(&env) - amount;
        Self::write_supply(&env, supply);

        events::emit_burn(&env, &from, amount, balance - amount, supply);
    }

    /// Returns the number of decimal places for the token.
    fn decimals(env: Env) -> u32 {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        env.storage()
            .instance()
            .get(&DataKey::Decimals)
            .unwrap_or(7)
    }

    /// Returns the human-readable token name.
    fn name(env: Env) -> String {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        env.storage()
            .instance()
            .get(&DataKey::Name)
            .unwrap_or_else(|| String::from_str(&env, "bc-forge"))
    }

    /// Returns the token ticker symbol.
    fn symbol(env: Env) -> String {
        Self::panic_on_err(&env, Self::ensure_initialized(&env));
        env.storage()
            .instance()
            .get(&DataKey::Symbol)
            .unwrap_or_else(|| String::from_str(&env, "SFG"))
    }
}
