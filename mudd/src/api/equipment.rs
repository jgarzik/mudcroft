//! Equipment: wield a weapon, wear armor
//!
//! An item names its slot in `slot` (an item with `damage` and no slot is a
//! weapon). Equipping marks it `equipped_by` its wearer; it counts as
//! equipped only while the wearer holds it, and a trigger clears the mark
//! whenever it moves (drop, give, death, a script). One item per slot.
//!
//! In combat (api/combat.rs) a wielded weapon's `damage` dice (e.g.
//! "1d8", "2d4+1"), `damage_type` and `attack_bonus` replace the bare 1d6
//! physical blow; each worn item adds its `armor_class` to the wearer's.

use serde_json::json;

use super::{AppState, ServerMessage};
use crate::objects::{Object, ObjectStore};

/// Slots that are worn (the weapon is wielded)
pub const WORN_SLOTS: &[&str] = &["head", "body", "hands", "feet", "shield", "neck", "ring"];

/// An item's slot, if it can be equipped
pub fn slot_of(item: &Object) -> Option<String> {
    match item.get_string("slot") {
        Some(slot) if slot == "weapon" || WORN_SLOTS.contains(&slot) => Some(slot.to_string()),
        Some(_) => None,
        None if item.properties.contains_key("damage") => Some("weapon".to_string()),
        None => None,
    }
}

/// What `wearer` has equipped (held and marked as theirs)
pub async fn equipped(state: &AppState, universe_id: &str, wearer: &str) -> Vec<Object> {
    state
        .object_store
        .get_contents(universe_id, wearer)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item.get_string("equipped_by") == Some(wearer) && slot_of(item).is_some())
        .collect()
}

/// The wielded weapon, if any
pub fn weapon(gear: &[Object]) -> Option<&Object> {
    gear.iter()
        .find(|i| slot_of(i).as_deref() == Some("weapon"))
}

/// Armor class worn items add
pub fn armor_bonus(gear: &[Object]) -> i32 {
    gear.iter()
        .filter(|i| slot_of(i).as_deref() != Some("weapon"))
        .filter_map(|i| i.properties.get("armor_class").and_then(|v| v.as_i64()))
        .map(|ac| ac.clamp(-1000, 1000) as i32)
        .fold(0i32, |a, b| a.saturating_add(b))
}

fn name_of(obj: &Object) -> String {
    obj.properties
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("something")
        .to_string()
}

async fn set_wearer(
    state: &AppState,
    universe_id: &str,
    item: &str,
    wearer: Option<&str>,
) -> anyhow::Result<()> {
    let statement = ObjectStore::set_properties_statement(
        universe_id,
        item,
        &[("equipped_by", json!(wearer))],
    )?;
    state.object_store.execute_batch(vec![statement]).await
}

/// Find an item the player carries by name (exact, else partial)
async fn carried(state: &AppState, universe_id: &str, player: &str, name: &str) -> Option<Object> {
    let name = name.trim().to_lowercase();
    let items = state
        .object_store
        .get_contents(universe_id, player)
        .await
        .unwrap_or_default();
    let named = |o: &Object| o.get_string("name").map(|n| n.to_lowercase());
    items
        .iter()
        .find(|o| named(o).as_deref() == Some(name.as_str()))
        .or_else(|| {
            items
                .iter()
                .find(|o| named(o).is_some_and(|n| n.contains(&name)))
        })
        .cloned()
}

fn error(message: impl Into<String>) -> ServerMessage {
    ServerMessage::Error {
        message: message.into(),
    }
}

fn output(text: impl Into<String>) -> ServerMessage {
    ServerMessage::Output { text: text.into() }
}

/// wield / wear: equip a carried item in its slot (replacing what was there)
pub async fn equip(
    state: &AppState,
    universe_id: &str,
    player: &str,
    name: &str,
    wield: bool,
) -> ServerMessage {
    let verb = if wield { "wield" } else { "wear" };
    if name.trim().is_empty() {
        return error(format!("{} what?", if wield { "Wield" } else { "Wear" }));
    }
    let Some(item) = carried(state, universe_id, player, name).await else {
        return error("You don't have that.");
    };
    let slot = slot_of(&item);
    let fits = match slot.as_deref() {
        Some("weapon") => wield,
        Some(_) => !wield,
        None => false,
    };
    let Some(slot) = slot.filter(|_| fits) else {
        return error(format!("You can't {} {}.", verb, name_of(&item)));
    };
    for old in equipped(state, universe_id, player).await {
        if old.id != item.id && slot_of(&old).as_deref() == Some(slot.as_str()) {
            if let Err(e) = set_wearer(state, universe_id, &old.id, None).await {
                return error(format!("Could not {}: {}", verb, e));
            }
        }
    }
    if let Err(e) = set_wearer(state, universe_id, &item.id, Some(player)).await {
        return error(format!("Could not {}: {}", verb, e));
    }
    output(format!("You {} {}.", verb, name_of(&item)))
}

/// unwield / remove: take off an equipped item (unwield: the weapon)
pub async fn unequip(
    state: &AppState,
    universe_id: &str,
    player: &str,
    name: Option<&str>,
) -> ServerMessage {
    let gear = equipped(state, universe_id, player).await;
    let item = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => {
            let name = name.to_lowercase();
            gear.iter()
                .find(|i| name_of(i).to_lowercase().contains(&name))
                .cloned()
        }
        None => weapon(&gear).cloned(),
    };
    let Some(item) = item else {
        return error("You aren't using that.");
    };
    if let Err(e) = set_wearer(state, universe_id, &item.id, None).await {
        return error(format!("Could not remove it: {}", e));
    }
    output(format!("You stop using {}.", name_of(&item)))
}

/// equipment: what is equipped, and the resulting armor class
pub async fn list(state: &AppState, universe_id: &str, player: &str) -> ServerMessage {
    let gear = equipped(state, universe_id, player).await;
    let base = state
        .object_store
        .get(universe_id, player)
        .await
        .ok()
        .flatten()
        .map(|p| crate::combat::engine::Combatant::from_object(&p, 100).armor_class)
        .unwrap_or(10);
    let mut lines = Vec::new();
    for slot in std::iter::once("weapon").chain(WORN_SLOTS.iter().copied()) {
        if let Some(item) = gear.iter().find(|i| slot_of(i).as_deref() == Some(slot)) {
            lines.push(format!("  {}: {}", slot, name_of(item)));
        }
    }
    if lines.is_empty() {
        lines.push("  (nothing)".to_string());
    }
    output(format!(
        "You are using:\n{}\nArmor class: {}",
        lines.join("\n"),
        base.saturating_add(armor_bonus(&gear))
    ))
}
