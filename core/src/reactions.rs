//! Per-user reactions. An empty snapshot clears reactions; missing data does not.
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn kind(value: &Value) -> Option<u8> {
    let n = value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .or_else(|| {
            ["UNDO", "NICE", "LOVE", "FUN", "AMAZING", "SAD", "OMG"]
                .iter()
                .position(|s| Some(*s) == value.as_str())
                .map(|i| i as u64 + 1)
        })?;
    (1..=7).contains(&n).then_some(n as u8)
}

pub fn normalize(value: &Value) -> Value {
    let mut users = BTreeMap::<String, (i64, u8)>::new();
    for row in value.as_array().into_iter().flatten() {
        let mid = row["fromUserMid"]
            .as_str()
            .or_else(|| row["1"].as_str())
            .unwrap_or_default();
        let reaction = kind(&row["reactionType"]["predefinedReactionType"])
            .or_else(|| kind(&row["3"]["1"]))
            .or_else(|| kind(&row["reaction"]));
        let Some(reaction) = reaction else { continue };
        if mid.is_empty() {
            continue;
        }
        let at = crate::message::integer(
            row.get("atMillis")
                .or_else(|| row.get("2"))
                .unwrap_or(&Value::Null),
        );
        if users.get(mid).is_none_or(|(old, _)| at >= *old) {
            users.insert(mid.into(), (at, reaction));
        }
    }
    json!(
        users
            .into_iter()
            .filter(|(_, (_, reaction))| *reaction != 1)
            .map(
                |(mid, (at, reaction))| json!({"fromUserMid":mid,"atMillis":at,"reaction":reaction})
            )
            .collect::<Vec<_>>()
    )
}
