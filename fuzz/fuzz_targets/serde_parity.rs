#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::{Deserialize, Serialize};
use tristr::TriStr;

#[derive(Serialize, Deserialize)]
struct W<T> {
    payload: T,
}

/// serde wire format must stay byte-identical to std String for any input.
/// (YAML parity is covered by the integration suite; libfuzzer exercises the
/// shared visitor contract via JSON.)
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    let a = serde_json::to_string(&W {
        payload: TriStr::from(s),
    })
    .unwrap();
    let b = serde_json::to_string(&W {
        payload: s.to_string(),
    })
    .unwrap();
    assert_eq!(a, b, "JSON wire parity with std String");

    let back: W<TriStr> = serde_json::from_str(&a).unwrap();
    assert_eq!(back.payload.as_str(), s);
});
