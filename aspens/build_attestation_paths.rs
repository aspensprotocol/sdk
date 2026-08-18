//! Pure string transformation used by `build.rs` to rewrite the
//! attestation type references emitted by tonic-prost-build inside the
//! generated `xyz.aspens.arborter_config.v1.rs`.
//!
//! tonic-prost-build emits cross-package type references relative to
//! the generated module's own location: from `arborter_config.v1` it
//! walks up three levels to what it assumes is the root of one proto
//! module tree, then back down into `attestation::v1::*`, giving
//! `super::super::super::attestation::v1::...`. Nothing in this crate
//! has that shape — the bindings are `include!`d at
//! `crate::commands::config::config_pb`, and the attestation ones at
//! `crate::attestation::v1`.
//!
//! That works, but it is *brittle*: any change to either module's
//! nesting silently miscompiles or paints the wrong types into the
//! generated client. Rewriting the relative path to an absolute
//! `crate::attestation::v1::...` makes the import depend only on the
//! top-level layout we control in `lib.rs`.
//!
//! The function lives in its own file (loaded into both `build.rs` and
//! the test target via `#[path]`) so it can be unit-tested without
//! having to invoke the full build script.
//!
//! Coupled to the layout in `aspens/src/lib.rs`. If the
//! `pub mod attestation { pub mod v1 { ... } }` location changes,
//! [`RELATIVE_PREFIX`] and [`ABSOLUTE_PREFIX`] must change in lock-step.
//! Because the rewritten path is absolute, moving the *config* bindings'
//! include site needs no change here — which is the point of the rewrite.

/// The relative path tonic-prost-build emits today, leading into
/// `attestation::v1::*` from the `arborter_config` generated module.
pub const RELATIVE_PREFIX: &str = "super::super::super::attestation::v1::";

/// The absolute path we want consumers to see — anchored at the crate
/// root so it can't drift with `arborter_config`'s nesting.
pub const ABSOLUTE_PREFIX: &str = "crate::attestation::v1::";

/// Rewrite every occurrence of [`RELATIVE_PREFIX`] in `content` to
/// [`ABSOLUTE_PREFIX`]. Pure; no I/O.
pub fn rewrite_attestation_paths(content: &str) -> String {
    content.replace(RELATIVE_PREFIX, ABSOLUTE_PREFIX)
}
