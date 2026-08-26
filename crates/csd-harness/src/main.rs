//! Validation harness (M0): clone a corpus, walk first-parent commit pairs, extract and diff
//! each pair across a rename-threshold sweep, and report spurious-split rate, false-merge audit,
//! and edge precision/recall. See `SPEC.md` section 6 and the backlog (area `harness`).
//!
//! Not yet implemented at v0.0.0.

use anyhow::Result;

fn main() -> Result<()> {
    eprintln!(
        "csd-harness v0.0.0: not yet implemented. See BACKLOG.md (area `harness`) and SPEC.md section 6."
    );
    Ok(())
}
