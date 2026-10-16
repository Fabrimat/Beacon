//! `marketplace`: the store submission zip (`Content/world_template/...`, `Store Art`,
//! `Marketing Art`). Not built yet.

use super::{ArtifactCx, unsupported};
use crate::build::Attempt;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    unsupported(cx, a);
}
