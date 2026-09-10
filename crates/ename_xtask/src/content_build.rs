//! `ename_content_build` -- stub.
//!
//! Bakes content to `content-index.ron` for shipping builds. Not implemented: the baked-index
//! format and the rest of the shipping-build path are phase 5 work (see
//! `scratch/content-addressing-design.md`). This subcommand exists now so the name is taken and
//! `cargo xtask ename_content_build` fails loudly rather than with "unknown subcommand" once
//! something depends on it existing.

pub fn content_build() {
    println!(
        "ename_content_build: not implemented yet (see scratch/content-addressing-design.md, phase 5)"
    );
}
