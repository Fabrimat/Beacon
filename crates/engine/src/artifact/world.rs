//! `mcworld` / `mctemplate`: the target's world with `level.dat` edits, pack lists and
//! (unless `embedPacks: false`) the outputs' packs. Not built yet.

use super::{ArtifactCx, unsupported};
use crate::build::Attempt;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    unsupported(cx, a);
}
