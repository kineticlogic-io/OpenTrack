//! The identity registry in SQLite.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::sqlite::{Db, Decision, Result, StoreError, now_ms, record_decision};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryIdentifier {
    pub scheme: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// An entity with its identifiers: the import/export and API shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "active")]
    pub status: String,
    #[serde(default)]
    pub fields: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub identifiers: Vec<RegistryIdentifier>,
}

fn active() -> String {
    "active".into()
}

/// One active identifier joined to its entity, for lookup snapshots.
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryRow {
    pub scheme: String,
    pub value: String,
    pub entity_id: String,
    pub name: Option<String>,
    pub expected_name: Option<String>,
    pub fields: Map<String, Value>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportCounts {
    pub entities_created: u64,
    pub entities_updated: u64,
    pub identifiers: u64,
    /// Identifiers skipped because another entity already holds them.
    pub conflicts: u64,
}

impl Db {
    /// Upsert entities and their identifiers. An identifier already held by a
    /// different entity is left alone and counted as a conflict, so an
    /// import can never silently move an identifier.
    pub fn registry_import(
        &mut self,
        entities: &[RegistryEntity],
        actor: &str,
        label: &str,
    ) -> Result<ImportCounts> {
        self.write(|tx| {
            let now = now_ms();
            let mut counts = ImportCounts::default();
            for e in entities {
                if !matches!(e.status.as_str(), "active" | "retired") {
                    return Err(StoreError::Conflict(format!(
                        "entity {}: status must be active or retired",
                        e.id
                    )));
                }
                let existed: bool = tx
                    .query_row("SELECT 1 FROM registry_entities WHERE id = ?1", [&e.id], |_| Ok(()))
                    .optional()?
                    .is_some();
                tx.execute(
                    "INSERT INTO registry_entities (id, name, status, fields, source, created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                     ON CONFLICT(id) DO UPDATE SET name = excluded.name, status = excluded.status,
                         fields = excluded.fields, source = excluded.source,
                         updated_at_ms = excluded.updated_at_ms",
                    params![e.id, e.name, e.status, Value::Object(e.fields.clone()).to_string(), e.source, now],
                )?;
                if existed {
                    counts.entities_updated += 1;
                } else {
                    counts.entities_created += 1;
                }
                for i in &e.identifiers {
                    let holder: Option<String> = tx
                        .query_row(
                            "SELECT entity_id FROM registry_identifiers WHERE scheme = ?1 AND value = ?2",
                            params![i.scheme, i.value],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match holder {
                        Some(h) if h != e.id => counts.conflicts += 1,
                        _ => {
                            tx.execute(
                                "INSERT INTO registry_identifiers (scheme, value, entity_id, expected_name, source, added_at_ms)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                                 ON CONFLICT(scheme, value) DO UPDATE SET expected_name = excluded.expected_name,
                                     source = excluded.source",
                                params![i.scheme, i.value, e.id, i.expected_name, i.source, now],
                            )?;
                            counts.identifiers += 1;
                        }
                    }
                }
            }
            let d = Decision::new(actor, "registry_import")
                .reason(label.to_owned())
                .evidence(serde_json::to_value(counts)?);
            record_decision(tx, &d, now)?;
            Ok(counts)
        })
    }

    /// Every identifier of every active entity.
    pub fn registry_rows(&self) -> Result<Vec<RegistryRow>> {
        let mut stmt = self.connection().prepare(
            "SELECT i.scheme, i.value, e.id, e.name, i.expected_name, e.fields
             FROM registry_identifiers i JOIN registry_entities e ON e.id = i.entity_id
             WHERE e.status = 'active'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        rows.map(|r| {
            let (scheme, value, entity_id, name, expected_name, fields) = r?;
            let fields = match serde_json::from_str(&fields)? {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            Ok(RegistryRow {
                scheme,
                value,
                entity_id,
                name,
                expected_name,
                fields,
            })
        })
        .collect()
    }

    /// Changes whenever the registry does; lookup snapshots poll it.
    pub fn registry_version(&self) -> Result<String> {
        Ok(self.connection().query_row(
            "SELECT (SELECT count(*) FROM registry_entities) || ':' ||
                    (SELECT coalesce(max(updated_at_ms), 0) FROM registry_entities) || ':' ||
                    (SELECT count(*) FROM registry_identifiers) || ':' ||
                    (SELECT coalesce(max(added_at_ms), 0) FROM registry_identifiers)",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn registry_entity(&self, id: &str) -> Result<Option<RegistryEntity>> {
        let Some((name, status, fields, source)) = self
            .connection()
            .query_row(
                "SELECT name, status, fields, source FROM registry_entities WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt = self.connection().prepare(
            "SELECT scheme, value, expected_name, source FROM registry_identifiers
             WHERE entity_id = ?1 ORDER BY scheme, value",
        )?;
        let identifiers = stmt
            .query_map([id], |r| {
                Ok(RegistryIdentifier {
                    scheme: r.get(0)?,
                    value: r.get(1)?,
                    expected_name: r.get(2)?,
                    source: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(Some(RegistryEntity {
            id: id.to_owned(),
            name,
            status,
            fields: match serde_json::from_str(&fields)? {
                Value::Object(m) => m,
                _ => Map::new(),
            },
            source,
            identifiers,
        }))
    }

    /// The entity an identifier resolves to, if any.
    pub fn registry_resolve(&self, scheme: &str, value: &str) -> Result<Option<RegistryEntity>> {
        let id: Option<String> = self
            .connection()
            .query_row(
                "SELECT entity_id FROM registry_identifiers WHERE scheme = ?1 AND value = ?2",
                params![scheme, value],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.registry_entity(&id),
            None => Ok(None),
        }
    }

    pub fn registry_counts(&self) -> Result<Value> {
        let (active, retired, ids): (i64, i64, i64) = self.connection().query_row(
            "SELECT (SELECT count(*) FROM registry_entities WHERE status = 'active'),
                    (SELECT count(*) FROM registry_entities WHERE status = 'retired'),
                    (SELECT count(*) FROM registry_identifiers)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(json!({ "active": active, "retired": retired, "identifiers": ids }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: &str, mmsi: &str) -> RegistryEntity {
        serde_json::from_value(json!({
            "id": id, "name": "USS EXAMPLE", "fields": {"cot": "a-f-S-C", "hull_code": "DDG-1"},
            "identifiers": [{"scheme": "mmsi", "value": mmsi, "expected_name": "EXAMPLE"}]
        }))
        .unwrap()
    }

    #[test]
    fn import_upserts_and_refuses_to_move_identifiers() {
        let mut db = Db::open_in_memory().unwrap();
        let c = db
            .registry_import(&[entity("e1", "1"), entity("e2", "2")], "op:test", "seed")
            .unwrap();
        assert_eq!((c.entities_created, c.identifiers, c.conflicts), (2, 2, 0));
        let v1 = db.registry_version().unwrap();

        // e3 claims e1's identifier: conflict, nothing moves.
        let c = db
            .registry_import(&[entity("e1", "1"), entity("e3", "1")], "op:test", "again")
            .unwrap();
        assert_eq!(
            (c.entities_created, c.entities_updated, c.conflicts),
            (1, 1, 1)
        );
        assert_eq!(db.registry_resolve("mmsi", "1").unwrap().unwrap().id, "e1");
        assert_ne!(db.registry_version().unwrap(), v1);

        let rows = db.registry_rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().find(|r| r.value == "1").unwrap().fields["cot"],
            "a-f-S-C"
        );

        let mut retired = entity("e2", "2");
        retired.status = "retired".into();
        db.registry_import(&[retired], "op:test", "retire").unwrap();
        assert_eq!(db.registry_rows().unwrap().len(), 1);
        assert_eq!(db.registry_counts().unwrap()["retired"], 1);
    }
}
