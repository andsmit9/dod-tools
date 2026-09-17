//! The one place this DLL's console prefix is spelled.
//!
//! Every cvar and command it registers goes through [`console_name`], so
//! renaming the whole surface — `dodtools_` to anything else — is a single edit
//! here rather than a hunt through five files for string literals that happen
//! to start the same way.
//!
//! It is a macro rather than a `const` + runtime `format!` because these names
//! are needed at compile time: they end up in `const` items and in the
//! `format!` strings of every usage and error message, where a runtime
//! concatenation would mean allocating, and would stop the names being usable
//! in `concat!`/inline-format position at all.
//!
//! ## Aliases
//!
//! Nothing here forces one name per feature. [`crate::commands::add_commands`]
//! takes a slice, so a command can be registered under several names at once —
//! a renamed command can keep its old name working for a release, and a variant
//! spelling costs one more array entry:
//!
//! ```ignore
//! add_commands(&[console_name!("deathmsg"), console_name!("killfeed")], command);
//! ```

/// Builds a console name from the shared prefix.
///
/// ```ignore
/// const STATUS: &str = console_name!("status");   // "dodtools_status"
/// ```
macro_rules! console_name {
    ($suffix:literal) => {
        concat!("dodtools_", $suffix)
    };
}

pub(crate) use console_name;
