use beacon_engine::canonical::to_vec;
use serde_json::json;

#[test]
fn snapshot() {
    let v = json!({"b": [1, 2.5, -3, 1e21, {}], "a": {"z": null, "y": true}, "s": "q\"\\\n\u{1}é😀", "e": []});
    let want = "{\n  \"a\": {\n    \"y\": true,\n    \"z\": null\n  },\n  \"b\": [\n    1,\n    2.5,\n    -3,\n    1e+21,\n    {}\n  ],\n  \"e\": [],\n  \"s\": \"q\\\"\\\\\\n\\u0001é😀\"\n}\n";
    assert_eq!(String::from_utf8(to_vec(&v)).unwrap(), want);
    assert_eq!(to_vec(&v), to_vec(&v));
}
