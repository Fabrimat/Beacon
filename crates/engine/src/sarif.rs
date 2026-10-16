//! Workstream A5: SARIF 2.1 writer and compact text view (6.3).

use crate::diag::Diagnostic;

/// SARIF 2.1.0 log with one run (tool `beacon`, rules from `Code`, results with locations,
/// related locations and fixes).
pub fn to_sarif(_diags: &[Diagnostic]) -> serde_json::Value {
    todo!("A5")
}

/// `path:line:col: error[BCN0011]: message`, one per line, sorted by path then line.
pub fn to_text(_diags: &[Diagnostic]) -> String {
    todo!("A5")
}
