//! `skinpack`: the target's `skins` folder as a skin pack (`.mcpack`). Not built yet.

use super::{ArtifactCx, unsupported};
use crate::build::Attempt;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    unsupported(cx, a);
}
