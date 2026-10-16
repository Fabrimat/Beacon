use beacon_engine::diag::{Code, Diagnostic, Fix};
use beacon_engine::path::RelPath;
use beacon_engine::sarif::{to_sarif, to_text};

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn sample() -> Vec<Diagnostic> {
    let late = Diagnostic::new(Code::RouteCollision, "dup")
        .at(rp("b/x.json"), Some(3), Some(4))
        .related(rp("a/y.json"));
    let late = Diagnostic {
        fixes: vec![Fix {
            description: "rename".into(),
            path: rp("b/x.json"),
            start: 2,
            end: 5,
            replacement: "z".into(),
            safe: false,
        }],
        ..late
    };
    vec![
        late,
        Diagnostic::new(Code::Untrusted, "code"),
        Diagnostic::new(Code::UnknownSuffix, "odd").at(rp("a/a.foo"), None, None),
    ]
}

#[test]
fn sarif_snapshot() {
    let v = to_sarif(&sample());
    assert_eq!(v["version"], "2.1.0");
    assert!(v["$schema"].as_str().unwrap().contains("sarif"));
    let run = &v["runs"][0];
    assert_eq!(run["tool"]["driver"]["name"], "beacon");
    assert_eq!(run["tool"]["driver"]["version"], env!("CARGO_PKG_VERSION"));
    let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
    let ids: Vec<_> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["BCN0010", "BCN0011", "BCN0050"]);
    for r in rules {
        assert!(r["shortDescription"]["text"].is_string());
        assert!(r["fullDescription"]["text"].is_string());
    }
    let res = run["results"].as_array().unwrap();
    // No span sorts first, then path order.
    let got: Vec<_> = res.iter().map(|r| r["ruleId"].as_str().unwrap()).collect();
    assert_eq!(got, ["BCN0050", "BCN0010", "BCN0011"]);
    for r in res {
        assert!(["error", "warning", "note"].contains(&r["level"].as_str().unwrap()));
        assert!(r["message"]["text"].is_string() && r["locations"].is_array());
    }
    let r = &res[2];
    let pl = &r["locations"][0]["physicalLocation"];
    assert_eq!(pl["artifactLocation"]["uri"], "b/x.json");
    assert_eq!(pl["region"]["startLine"], 3);
    assert_eq!(pl["region"]["startColumn"], 4);
    assert_eq!(
        r["relatedLocations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "a/y.json"
    );
    let rep = &r["fixes"][0]["artifactChanges"][0]["replacements"][0];
    assert_eq!(rep["deletedRegion"]["byteOffset"], 2);
    assert_eq!(rep["deletedRegion"]["byteLength"], 3);
    assert_eq!(rep["insertedContent"]["text"], "z");
    assert_eq!(r["fixes"][0]["properties"]["safe"], false);
    assert!(
        res[1]["locations"][0]["physicalLocation"]
            .get("region")
            .is_none()
    );
}

#[test]
fn text_sorted() {
    assert_eq!(
        to_text(&sample()),
        "error[BCN0050]: code\na/a.foo: warning[BCN0010]: odd\nb/x.json:3:4: error[BCN0011]: dup\n"
    );
}
