//! Shared CLI helpers for the Aspens binaries.
//!
//! Before this crate existed, [`format_error`] was triplicated across
//! `aspens-cli/src/main.rs`, `aspens-repl/src/main.rs`, and
//! `aspens-admin/src/main.rs` — three ~250-line copies that drifted on
//! every UX tweak. Same for [`resolve_token_amount`] (cli + repl).
//!
//! Each binary now passes a [`BinaryContext`] describing its own name
//! and the env-var holding its private key; the shared helpers
//! interpolate those into hint messages.

mod amount;
mod error;

pub use amount::resolve_token_amount;
pub use error::format_error;

/// Per-binary parameters used to customize hint messages from the
/// shared CLI helpers.
#[derive(Debug, Clone, Copy)]
pub struct BinaryContext {
    /// Binary name as it appears in user-facing hints (e.g.
    /// `"aspens-cli"`, `"aspens-repl"`, `"aspens-admin"`). Used to
    /// render commands the user can copy-paste, like
    /// `"Check server status with 'aspens-cli status'"`.
    pub name: &'static str,

    /// Name of the env var that holds this binary's signing key
    /// (`"TRADER_PRIVKEY"` for cli/repl, `"ADMIN_PRIVKEY"` for admin).
    /// Surfaced in the "Invalid private key" hint branch.
    pub privkey_env_var: &'static str,
}

impl BinaryContext {
    /// Shorthand for the cli/repl trader-flavored binary context.
    pub const TRADER_CLI: BinaryContext = BinaryContext {
        name: "aspens-cli",
        privkey_env_var: "TRADER_PRIVKEY",
    };

    /// Shorthand for the repl trader-flavored binary context.
    pub const TRADER_REPL: BinaryContext = BinaryContext {
        name: "aspens-repl",
        privkey_env_var: "TRADER_PRIVKEY",
    };

    /// Shorthand for the admin binary context.
    pub const ADMIN: BinaryContext = BinaryContext {
        name: "aspens-admin",
        privkey_env_var: "ADMIN_PRIVKEY",
    };
}
