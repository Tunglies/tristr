#![no_main]

use libfuzzer_sys::fuzz_target;
use std::hint::black_box;
use std::ops::DerefMut;
use tristr::TriStr;

// Random mutation sequences against a std `String` oracle: construct a
// value in any of the three reprs (inline / owned Box / shared Arc), then
// apply a data-driven chain of `make_ascii_uppercase`,
// `make_ascii_lowercase`, `push_str`, `push(char)` and byte-level
// rewrites through `DerefMut` (`replace_range` on one ASCII byte),
// intermixed with clone-then-mutate COW probes. Content, length, repr
// routing and sibling isolation are re-verified after every step.
fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let route = data[0] % 3;
    let payload = &data[1..];
    let split = payload.len() / 2;
    let Ok(text) = std::str::from_utf8(&payload[..split]) else {
        return;
    };

    // Route across all three reprs: inline/short, owned (exact-cap Box) and
    // shared (slack-capacity String -> Arc copy).
    let mut v = match route {
        0 => TriStr::from(text),             // inline or shared
        1 => TriStr::from(text.to_owned()),  // inline or owned
        _ => {
            let mut slack = String::with_capacity(text.len() + 64);
            slack.push_str(text);
            TriStr::from(slack) // inline or shared
        }
    };
    let mut model = text.to_string();
    black_box(&v);

    let parity = |v: &TriStr, model: &str, ctx: &str| {
        assert_eq!(v.is_inline(), model.len() <= 23, "repr routing ({ctx})");
        assert_eq!(v.len(), model.len(), "len ({ctx})");
        assert_eq!(v.as_str(), model, "content ({ctx})");
        assert_eq!(v.as_bytes(), model.as_bytes(), "bytes ({ctx})");
    };
    parity(&v, &model, "initial");

    // Ops stream: one (selector, operand) pair per two bytes.
    let mut steps = 0usize;
    for pair in payload[split..].chunks(2) {
        let &[sel, arg] = pair else { break };
        steps += 1;
        let before = model.clone();
        let sibling = v.clone();

        match sel % 6 {
            0 => {
                v.make_ascii_uppercase();
                model.make_ascii_uppercase();
            }
            1 => {
                v.make_ascii_lowercase();
                model.make_ascii_lowercase();
            }
            2 => {
                let frag = [b'a' + (arg % 26), b'0' + (arg % 10)];
                let frag = std::str::from_utf8(&frag[..1 + (arg as usize % 2)]).unwrap();
                v.push_str(frag);
                model.push_str(frag);
            }
            3 => {
                let ch = match arg % 5 {
                    0 => 'a',
                    1 => 'Z',
                    2 => '0',
                    3 => 'é', // 2-byte
                    _ => '🦀', // 4-byte
                };
                v.push(ch);
                model.push(ch);
            }
            4 => {
                // Byte-level rewrite through DerefMut: case-flip a
                // char-bounded sub-range in place (inline/owned write
                // through the payload; shared COW-copies first). The oracle
                // applies the identical slice mutation via its own
                // Deref<Target = str>.
                let mut bounds: Vec<usize> = model.char_indices().map(|(i, _)| i).collect();
                bounds.push(model.len());
                let p = bounds[(arg as usize) % bounds.len()];
                let q = bounds[(arg as usize / 3 + 1) % bounds.len()];
                let (a, b) = (p.min(q), p.max(q));
                let upper = arg % 2 == 0;
                let m: &mut str = &mut model;
                if upper {
                    m[a..b].make_ascii_uppercase();
                } else {
                    m[a..b].make_ascii_lowercase();
                }
                let vs: &mut str = v.deref_mut();
                if upper {
                    vs[a..b].make_ascii_uppercase();
                } else {
                    vs[a..b].make_ascii_lowercase();
                }
            }
            _ => {
                // Clone-then-mutate: the sibling must never observe writes,
                // whichever repr either side is in.
                let mut c = v.clone();
                c.make_ascii_uppercase();
                assert_eq!(sibling.as_str(), before, "COW leak into sibling ({steps})");
                assert_eq!(v.as_str(), before, "COW leak into source ({steps})");
                parity(&c, &before.to_ascii_uppercase(), "clone-mutate");
                black_box(&c);
                continue;
            }
        }

        assert_eq!(sibling.as_str(), before, "COW isolation ({steps})");
        parity(&v, &model, &format!("step {steps}"));
        // A fresh shared-construction copy of the current content agrees.
        assert_eq!(TriStr::from(v.as_str()).as_str(), model);
        black_box(&v);
    }

    parity(&v, &model, "final");
    assert_eq!(String::from(v.clone()), model);
    assert_eq!(v.capacity(), if model.len() <= 23 { 23 } else { model.len() });
});
