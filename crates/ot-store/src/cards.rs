//! Baseball cards: values an admin enters for an entity's output schema
//! fields (a ship's contact phone, say), which no feed provides. The server
//! checks values against the schema before they get here. Every save is kept
//! as a revision and recorded in the decision log.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::registry::{RegistryEntity, RegistryIdentifier, normalize_scheme};
use crate::sqlite::{Db, Decision, Result, StoreError, now_ms, record_decision};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Card {
    pub entity_id: String,
    pub values: Map<String, Value>,
    /// Output schema version the values were checked against.
    pub schema_version: u32,
    pub updated_at_ms: i64,
    pub decision_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardRevision {
    pub id: i64,
    pub values: Map<String, Value>,
    pub schema_version: u32,
    pub saved_at_ms: i64,
    pub decision_id: Option<i64>,
    pub actor: Option<String>,
}

fn object(text: &str) -> Result<Map<String, Value>> {
    Ok(match serde_json::from_str(text)? {
        Value::Object(m) => m,
        _ => Map::new(),
    })
}

/// A short, readable id for an entity created from the UI.
fn new_entity_id() -> String {
    let t = chrono::Utc::now().timestamp_micros() as u64;
    let mut n = t ^ (u64::from(std::process::id()) << 40);
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = Vec::new();
    while n > 0 {
        s.push(ALPHABET[(n % 36) as usize]);
        n /= 36;
    }
    s.reverse();
    format!("ent-{}", String::from_utf8(s).unwrap_or_default())
}

impl Db {
    pub fn card(&self, entity_id: &str) -> Result<Option<Card>> {
        self.connection()
            .query_row(
                "SELECT card_values, schema_version, updated_at_ms, decision_id
                 FROM entity_cards WHERE entity_id = ?1",
                [entity_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u32>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?
            .map(|(values, schema_version, updated_at_ms, decision_id)| {
                Ok(Card {
                    entity_id: entity_id.to_owned(),
                    values: object(&values)?,
                    schema_version,
                    updated_at_ms,
                    decision_id,
                })
            })
            .transpose()
    }

    /// Replace an entity's card values (already checked against
    /// `schema_version`). Returns the saved card.
    pub fn put_card(
        &mut self,
        entity_id: &str,
        values: &Map<String, Value>,
        schema_version: u32,
        actor: &str,
    ) -> Result<Card> {
        if self.registry_entity(entity_id)?.is_none() {
            return Err(StoreError::NotFound(format!("entity {entity_id}")));
        }
        let before = self.card(entity_id)?.map(|c| c.values).unwrap_or_default();
        self.write(|tx| {
            let now = now_ms();
            let changed: Vec<&String> = values
                .keys()
                .chain(before.keys())
                .filter(|k| values.get(*k) != before.get(*k))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let d = Decision::new(actor, "edit_card")
                .evidence(json!({ "entity": entity_id, "changed": changed }))
                .before(Value::Object(before.clone()))
                .after(Value::Object(values.clone()));
            let decision_id = record_decision(tx, &d, now)?;
            let text = Value::Object(values.clone()).to_string();
            tx.execute(
                "INSERT INTO entity_cards (entity_id, card_values, schema_version, updated_at_ms, decision_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (entity_id) DO UPDATE SET card_values = ?2, schema_version = ?3,
                    updated_at_ms = ?4, decision_id = ?5",
                params![entity_id, text, schema_version, now, decision_id],
            )?;
            tx.execute(
                "INSERT INTO entity_card_revisions (entity_id, card_values, schema_version, saved_at_ms, decision_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![entity_id, text, schema_version, now, decision_id],
            )?;
            Ok(())
        })?;
        self.card(entity_id)?
            .ok_or_else(|| StoreError::NotFound(format!("card {entity_id}")))
    }

    /// An entity's card revisions, newest first.
    pub fn card_revisions(&self, entity_id: &str) -> Result<Vec<CardRevision>> {
        let mut stmt = self.connection().prepare(
            "SELECT r.id, r.card_values, r.schema_version, r.saved_at_ms, r.decision_id, d.actor
             FROM entity_card_revisions r LEFT JOIN decisions d ON d.id = r.decision_id
             WHERE r.entity_id = ?1 ORDER BY r.id DESC",
        )?;
        let rows = stmt.query_map([entity_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        rows.map(|r| {
            let (id, values, schema_version, saved_at_ms, decision_id, actor) = r?;
            Ok(CardRevision {
                id,
                values: object(&values)?,
                schema_version,
                saved_at_ms,
                decision_id,
                actor,
            })
        })
        .collect()
    }

    /// Every card's values, for the engine's snapshot.
    pub fn all_cards(&self) -> Result<Vec<(String, Map<String, Value>)>> {
        let mut stmt = self
            .connection()
            .prepare("SELECT entity_id, card_values FROM entity_cards")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (id, values) = r?;
            Ok((id, object(&values)?))
        })
        .collect()
    }

    /// Changes whenever a card or the published output schema does.
    pub fn cards_version(&self) -> Result<String> {
        Ok(self.connection().query_row(
            "SELECT (SELECT count(*) FROM entity_cards) || ':' ||
                    (SELECT coalesce(max(updated_at_ms), 0) FROM entity_cards) || ':' ||
                    (SELECT coalesce(max(version), 0) FROM schema_versions WHERE status = 'published')",
            [],
            |r| r.get(0),
        )?)
    }

    /// Create an entity (to hang a card on) from a name and identifiers, e.g.
    /// a live track's. Fails if another entity already holds an identifier.
    pub fn create_entity(
        &mut self,
        name: Option<&str>,
        identifiers: &[RegistryIdentifier],
        actor: &str,
    ) -> Result<RegistryEntity> {
        let mut ids = Vec::new();
        for i in identifiers {
            let scheme = normalize_scheme(&i.scheme)?;
            let value = i.value.trim().to_owned();
            if value.is_empty() {
                return Err(StoreError::Conflict(format!(
                    "identifier of scheme {scheme} has no value"
                )));
            }
            if let Some(holder) = self.registry_resolve(&scheme, &value)? {
                return Err(StoreError::Conflict(format!(
                    "{scheme}:{value} already belongs to entity {}",
                    holder.id
                )));
            }
            ids.push((scheme, value, i.expected_name.clone()));
        }
        let id = new_entity_id();
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        self.write(|tx| {
            let now = now_ms();
            let d = Decision::new(actor, "create_entity").evidence(json!({
                "entity": id,
                "name": name,
                "identifiers": ids.iter().map(|(s, v, _)| format!("{s}:{v}")).collect::<Vec<_>>(),
            }));
            record_decision(tx, &d, now)?;
            tx.execute(
                "INSERT INTO registry_entities (id, name, source, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id, name, actor, now],
            )?;
            for (scheme, value, expected) in &ids {
                tx.execute(
                    "INSERT INTO registry_identifiers (scheme, value, entity_id, expected_name, source, added_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![scheme, value, id, expected.as_deref().or(name), actor, now],
                )?;
            }
            Ok(())
        })?;
        self.registry_entity(&id)?
            .ok_or_else(|| StoreError::NotFound(format!("entity {id}")))
    }

    /// Entities whose name or any identifier value contains `q`
    /// (case-insensitive), with whether each has a card.
    pub fn search_entities(&self, q: &str, limit: usize) -> Result<Vec<(RegistryEntity, bool)>> {
        let pattern = format!("%{}%", q.trim().replace(['%', '_'], ""));
        let mut stmt = self.connection().prepare(
            "SELECT DISTINCT e.id FROM registry_entities e
             LEFT JOIN registry_identifiers i ON i.entity_id = e.id
             WHERE e.name LIKE ?1 COLLATE NOCASE OR i.value LIKE ?1 COLLATE NOCASE OR e.id = ?2
             ORDER BY e.name LIMIT ?3",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![pattern, q.trim(), limit as i64], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        ids.into_iter()
            .filter_map(|id| self.registry_entity(&id).transpose())
            .map(|e| {
                let e = e?;
                let has_card = self.card(&e.id)?.is_some_and(|c| !c.values.is_empty());
                Ok((e, has_card))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(scheme: &str, value: &str) -> RegistryIdentifier {
        RegistryIdentifier {
            scheme: scheme.into(),
            value: value.into(),
            expected_name: None,
            source: None,
        }
    }

    #[test]
    fn create_entity_edit_card_and_history() {
        let mut db = Db::open_in_memory().unwrap();
        let v0 = db.cards_version().unwrap();
        let e = db
            .create_entity(
                Some("TED STEVENS"),
                &[ident("MMSI", "338924210"), ident("imo", "9876543")],
                "op:test",
            )
            .unwrap();
        assert!(e.id.starts_with("ent-"));
        assert_eq!(e.identifiers.len(), 2);
        assert_eq!(e.identifiers[0].scheme, "imo");
        assert_eq!(
            e.identifiers[1].expected_name.as_deref(),
            Some("TED STEVENS")
        );

        // An identifier can only belong to one entity.
        assert!(matches!(
            db.create_entity(None, &[ident("mmsi", "338924210")], "op:test"),
            Err(StoreError::Conflict(_))
        ));

        let first = json!({"contact_phone": "+1 555 0100"});
        db.put_card(&e.id, first.as_object().unwrap(), 3, "op:alice")
            .unwrap();
        let second = json!({"contact_phone": "+1 555 0199", "destination": "LONG BEACH"});
        let card = db
            .put_card(&e.id, second.as_object().unwrap(), 3, "op:bob")
            .unwrap();
        assert_eq!(Value::Object(card.values.clone()), second);
        assert_ne!(db.cards_version().unwrap(), v0);

        let revs = db.card_revisions(&e.id).unwrap();
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0].actor.as_deref(), Some("op:bob"));
        assert_eq!(Value::Object(revs[1].values.clone()), first);
        assert_eq!(db.all_cards().unwrap().len(), 1);

        let hits = db.search_entities("stevens", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].1);
        assert_eq!(db.search_entities("9876", 10).unwrap().len(), 1);
        assert!(db.search_entities("nothing", 10).unwrap().is_empty());

        assert!(matches!(
            db.put_card("ent-missing", &Map::new(), 3, "op:test"),
            Err(StoreError::NotFound(_))
        ));
    }
}
