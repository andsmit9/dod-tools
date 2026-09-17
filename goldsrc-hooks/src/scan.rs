//! Signature scanning: finding code by what it *is* rather than where it was.
//!
//! This is how HLAE locates everything it patches in `hw.dll` and `client.dll`,
//! and it is deliberately copied here — see `docs/goldsrc_death_notices.md`.
//! `AfxHookGoldSrc.dll` carries a pattern database keyed by names like
//! `cstrike_CHudDeathNotice_Draw_YRes`, scans the loaded module for each, and
//! records the match's address and length.
//!
//! The advantage over a fixed RVA is not portability across mods — a pattern is
//! just as mod-specific — but that a pattern *fails loudly on the wrong build*
//! instead of silently addressing the wrong instruction. A wrong RVA is a
//! plausible number; a pattern that does not match is an error.
//!
//! ## Uniqueness is required here, unlike in HLAE
//!
//! [`find_unique`] refuses a pattern that matches more than once, rather than
//! taking the first hit. A pattern meant to identify one site and matching two
//! is a pattern that has not been proven, and picking the lower address would
//! turn that into a silent mispatch — the exact failure mode the scan is
//! supposed to remove.

use crate::pe;

/// A parsed HLAE-style byte signature: `None` is `??`, a wildcard.
pub struct Pattern(Vec<Option<u8>>);

impl Pattern {
    /// Parses `"A1 ?? ?? ?? ?? C7 44 24 04"` — the same spelling HLAE's own
    /// pattern database uses, so a signature can be moved between the two
    /// projects unchanged.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut bytes = Vec::new();
        for token in text.split_whitespace() {
            if token == "??" {
                bytes.push(None);
            } else {
                // Exactly two digits. `from_str_radix` alone would take "1" as
                // 0x01, so "a 1" -- a fumbled "a1" -- would parse as a valid
                // two-byte pattern and quietly match the wrong thing.
                let byte = (token.len() == 2)
                    .then(|| u8::from_str_radix(token, 16).ok())
                    .flatten()
                    .ok_or_else(|| format!("{token:?} is not a two-digit hex byte or `??`"))?;
                bytes.push(Some(byte));
            }
        }
        match bytes.first() {
            None => Err("the pattern is empty".to_string()),
            // A leading wildcard buys nothing and makes every match start one
            // byte earlier than the author meant.
            Some(None) => Err("the pattern starts with a wildcard".to_string()),
            Some(Some(_)) => Ok(Self(bytes)),
        }
    }

    /// Total rather than partial: a window with too few bytes left simply does
    /// not match, instead of indexing past the end. The one caller bounds its
    /// loop correctly, which is exactly the sort of thing that stays true only
    /// until someone adds a second caller.
    fn matches_at(&self, haystack: &[u8], at: usize) -> bool {
        haystack
            .get(at..at + self.0.len())
            .is_some_and(|window| {
                self.0.iter().zip(window).all(|(want, &got)| want.is_none_or(|b| got == b))
            })
    }
}

/// The one address in `module`'s executable sections matching `pattern`.
///
/// Errors if there is no match, or more than one — see the module docs for why
/// "more than one" is a failure rather than a choice.
///
/// Safety: `module` must be a fully-mapped PE image that stays mapped for the
/// call.
pub unsafe fn find_unique(module: usize, pattern: &str) -> Result<usize, String> {
    let pattern = Pattern::parse(pattern)?;
    let (start, len) = unsafe { pe::code_range(module as *mut u8) }
        .ok_or_else(|| "could not find an executable section in the module".to_string())?;
    // Safety: the caller guarantees the image is mapped; `code_range` returns a
    // span inside it.
    let code = unsafe { std::slice::from_raw_parts((module + start) as *const u8, len) };

    if code.len() < pattern.0.len() {
        return Err("the module's code is smaller than the pattern".to_string());
    }
    let first = pattern.0[0].expect("parse rejects a leading wildcard");
    let mut found: Option<usize> = None;
    for at in 0..=(code.len() - pattern.0.len()) {
        if code[at] != first || !pattern.matches_at(code, at) {
            continue;
        }
        let address = module + start + at;
        match found {
            None => found = Some(address),
            Some(earlier) => {
                return Err(format!(
                    "the pattern is not unique -- it matches at least +{:#x} and +{:#x}",
                    earlier - module,
                    address - module
                ));
            }
        }
    }
    found.ok_or_else(|| "the pattern does not match this build".to_string())
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    fn matches(pattern: &str, haystack: &[u8]) -> Vec<usize> {
        let p = Pattern::parse(pattern).expect("valid pattern");
        (0..=haystack.len().saturating_sub(p.0.len()))
            .filter(|&at| p.matches_at(haystack, at))
            .collect()
    }

    #[test]
    fn a_wildcard_matches_any_byte_but_still_has_to_be_there() {
        assert_eq!(matches("aa ?? cc", &[0xaa, 0x00, 0xcc, 0xaa, 0xff, 0xcc]), vec![0, 3]);
        // The wildcard occupies a position; it does not mean "anything or nothing".
        assert_eq!(matches("aa ?? cc", &[0xaa, 0xcc]), Vec::<usize>::new());
    }

    #[test]
    fn a_pattern_is_rejected_rather_than_quietly_reinterpreted() {
        for bad in ["", "   ", "?? aa", "zz", "aa 1"] {
            assert!(Pattern::parse(bad).is_err(), "{bad:?} should not parse");
        }
        assert!(Pattern::parse("A1 ?? ?? ?? ?? C7 44 24 04").is_ok());
    }

    #[test]
    fn case_does_not_matter_in_a_signature() {
        assert_eq!(matches("A1 c7", &[0xa1, 0xc7]), vec![0]);
        assert_eq!(matches("a1 C7", &[0xa1, 0xc7]), vec![0]);
    }
}
