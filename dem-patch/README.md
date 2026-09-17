# dem (vendored, patched fork)

> **This is `dod-studio`' vendored fork of the upstream `dem` crate** (pulled in via
> `[patch.crates-io]` in the workspace root `Cargo.toml`, not published to crates.io
> as this fork). The badges/links below describe the *upstream* project this was
> forked from — see the root [`README.md`](../README.md)'s "Why the fork of `dem`"
> section for what's actually changed here and why (the published crate `.unwrap()`s
> delta-decoder table lookups at 29 call sites across 7 files, so a malformed or
> unexpected demo panics the whole process instead of returning a parse error;
> this fork converts those to proper `nom` parse errors instead).

---

# dem

[![crates.io](https://img.shields.io/crates/v/dem)](https://crates.io/crates/dem) [![docs.rs](https://img.shields.io/docsrs/dem/latest?logo=brightgreen&link=https%3A%2F%2Fdocs.rs%2Fdem%2Flatest)](https://docs.rs/dem)


A complete GoldSrc demo parser and writer library

## Example

```rust
let mut demo = open_demo("./src/tests/demotest.dem").unwrap();

for entry in &mut demo.directory.entries {
    for frame in &mut entry.frames {
        if let FrameData::NetworkMessage(ref mut box_type) = &mut frame.frame_data {
            let data = &mut box_type.as_mut().1;
            
            if let MessageData::Parsed(messages) = &mut data.messages {
                messages.push(NetMessage::EngineMessage(Box::new(EngineMessage::SvcBad)));
            };
        }
    }
}

demo.write_to_file("./src/tests/demo2test.dem").unwrap();
```

## Acknowledgement

[hlviewer.js](https://github.com/skyrim/hlviewer.js)

[talent](https://github.com/cgdangelo/talent/tree/main)

[coldemoplayer](https://github.com/jpcy/coldemoplayer)

[hldemojs](https://github.com/Matherunner/hldemojs)