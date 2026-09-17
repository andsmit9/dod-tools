use crate::localization::translate_key;
use dod::Weapon;

/// Every `Weapon` variant paired with the `weapon.*` loc-file key that names
/// it in `localizations/dod_studio_english.txt`. The single list both
/// [`weapon_display_name`] and [`all_weapon_display_names`] draw from, so a
/// resolved name can never disagree with itself between the two.
const WEAPON_KEYS: &[(Weapon, &str)] = &[
    (Weapon::Unknown, "weapon.unknown"),
    (Weapon::Kabar, "weapon.kabar"),
    (Weapon::GermanKnife, "weapon.germanknife"),
    (Weapon::M1911, "weapon.m1911"),
    (Weapon::Luger, "weapon.luger"),
    (Weapon::Garand, "weapon.garand"),
    (Weapon::ScopedK98, "weapon.scopedk98"),
    (Weapon::Thompson, "weapon.thompson"),
    (Weapon::Stg44, "weapon.stg44"),
    (Weapon::Springfield, "weapon.springfield"),
    (Weapon::K98, "weapon.k98"),
    (Weapon::Bar, "weapon.bar"),
    (Weapon::Mp40, "weapon.mp40"),
    (Weapon::Mk2Grenade, "weapon.mk2grenade"),
    (Weapon::StickGrenade, "weapon.stickgrenade"),
    (Weapon::Mg42, "weapon.mg42"),
    (Weapon::Browning30Cal, "weapon.browning30cal"),
    (Weapon::Spade, "weapon.spade"),
    (Weapon::M1Carbine, "weapon.m1carbine"),
    (Weapon::Mg34, "weapon.mg34"),
    (Weapon::GreaseGun, "weapon.greasegun"),
    (Weapon::Fg42, "weapon.fg42"),
    (Weapon::K43, "weapon.k43"),
    (Weapon::LeeEnfield, "weapon.leeenfield"),
    (Weapon::Sten, "weapon.sten"),
    (Weapon::Bren, "weapon.bren"),
    (Weapon::Webley, "weapon.webley"),
    (Weapon::Bazooka, "weapon.bazooka"),
    (Weapon::Panzerschreck, "weapon.panzerschreck"),
    (Weapon::Piat, "weapon.piat"),
    (Weapon::Mortar, "weapon.mortar"),
    (Weapon::ScopedFg42, "weapon.scopedfg42"),
    (Weapon::M1A1Carbine, "weapon.m1a1carbine"),
    (Weapon::K98Bayonet, "weapon.k98bayonet"),
    (Weapon::ScopedLeeEnfield, "weapon.scopedleeenfield"),
    (Weapon::MillsBomb, "weapon.millsbomb"),
    (Weapon::BritishKnife, "weapon.britishknife"),
    (Weapon::ButtStock, "weapon.buttstock"),
    (Weapon::EnfieldBayonet, "weapon.enfieldbayonet"),
];

/// Resolves a weapon's real display name from the loc files (e.g.
/// `ScopedK98` -> "Scoped K98"). The single source of truth for weapon
/// text — `native::patch::scanner`'s kill-streak timeline and the frontend's
/// weapon tables (via [`all_weapon_display_names`]) both go through this
/// instead of independently deriving a name, which is what let the same
/// weapon show different text in different panes (issue #34).
pub fn weapon_display_name(weapon: &Weapon) -> String {
    WEAPON_KEYS
        .iter()
        .find(|(w, _)| w == weapon)
        .and_then(|(_, key)| translate_key(key))
        .unwrap_or_default()
}

/// Every weapon's raw JSON tag (as serde serializes a bare `Weapon` value,
/// e.g. `"ScopedK98"` — identical to `Debug`'s output for a unit variant,
/// since `Weapon` carries no `#[serde(rename...)]`) mapped to its resolved
/// display name. Meant to be shipped to the frontend once (e.g. over a Tauri
/// command) so it can look up the same names `weapon_display_name` produces
/// without duplicating the loc-key table in JS.
pub fn all_weapon_display_names() -> std::collections::HashMap<String, String> {
    WEAPON_KEYS
        .iter()
        .map(|(w, key)| (format!("{:?}", w), translate_key(key).unwrap_or_default()))
        .collect()
}
