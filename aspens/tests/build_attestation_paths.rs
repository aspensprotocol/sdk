//! Unit tests for the pure string transform used by `aspens/build.rs`
//! to rewrite attestation type paths in the generated arborter_config
//! Rust file.
//!
//! The transform lives in `aspens/build_attestation_paths.rs` and is
//! shared by `build.rs` (via `mod build_attestation_paths;`) and this
//! test target (via `#[path]`). Coupling the test to the source via
//! `#[path]` keeps a single source of truth and avoids duplicating the
//! constants.

#[path = "../build_attestation_paths.rs"]
mod build_attestation_paths;

use build_attestation_paths::{ABSOLUTE_PREFIX, RELATIVE_PREFIX, rewrite_attestation_paths};

#[test]
fn rewrites_single_occurrence() {
    let input = format!("{}GetAttestationRequest,", RELATIVE_PREFIX);
    let expected = format!("{}GetAttestationRequest,", ABSOLUTE_PREFIX);
    assert_eq!(rewrite_attestation_paths(&input), expected);
}

#[test]
fn rewrites_every_occurrence() {
    // The real generated file has both a request and a response type;
    // make sure the transform isn't accidentally first-match-only.
    let input = format!("tx: {}Req,\nrx: {}Resp,", RELATIVE_PREFIX, RELATIVE_PREFIX);
    let out = rewrite_attestation_paths(&input);
    assert!(
        !out.contains(RELATIVE_PREFIX),
        "no relative attestation prefix should remain after rewrite; got: {out}"
    );
    assert_eq!(out.matches(ABSOLUTE_PREFIX).count(), 2);
}

#[test]
fn passthrough_when_prefix_is_absent() {
    // tonic-prost-build may eventually change its emit and stop using
    // the relative form. The rewrite must be a no-op in that case
    // rather than mangling unrelated `crate::` paths.
    let input = "use crate::attestation::v1::GetAttestationRequest;";
    assert_eq!(rewrite_attestation_paths(input), input);
}

#[test]
fn rewrite_is_idempotent() {
    // build.rs runs unconditionally; if a previous run already
    // rewrote the file, the second run must leave it alone.
    let input = format!("{}AttestationDoc,", RELATIVE_PREFIX);
    let once = rewrite_attestation_paths(&input);
    let twice = rewrite_attestation_paths(&once);
    assert_eq!(once, twice);
    // And the second run produces no further relative-prefix matches.
    assert!(!twice.contains(RELATIVE_PREFIX));
}

#[test]
fn does_not_touch_partial_matches() {
    // A shorter walk-up (`super::super::attestation::v1::`) or a
    // different module name must not match — that would silently
    // corrupt unrelated generated code if the proto layout changes.
    let input = "x: super::super::attestation::v1::Foo;";
    assert_eq!(rewrite_attestation_paths(input), input);

    let input = "y: super::super::super::otherpkg::v1::Bar;";
    assert_eq!(rewrite_attestation_paths(input), input);
}

#[test]
fn current_generated_file_uses_absolute_prefix_only() {
    // Regression guard: after build.rs runs, the on-disk generated
    // file must contain only `crate::attestation::v1::...` references.
    // If this fails, either build.rs didn't run, or the rewrite is
    // mis-targeted, or tonic-prost-build's emit changed shape.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/proto/generated/xyz.aspens.arborter_config.v1.rs",
    );
    let content =
        std::fs::read_to_string(path).expect("generated arborter_config file should exist");
    assert!(
        !content.contains(RELATIVE_PREFIX),
        "{} contains relative attestation prefix — build.rs rewrite drifted",
        path,
    );
    assert!(
        content.contains(ABSOLUTE_PREFIX),
        "{} has no absolute attestation references — sanity-check the proto",
        path,
    );
}
