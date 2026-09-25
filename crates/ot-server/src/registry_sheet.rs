//! The registry as a spreadsheet: one row per entity, for export and bulk
//! import (CSV, or XLSX from any spreadsheet program).
//!
//! Columns: `entity_id`, `name`, `status`; `id:<scheme>` for each identifier
//! scheme (several values of one scheme separated by `;`); `registry:<key>`
//! for the entity's registry fields (cot, flag, hull_code...); and
//! `card:<field>` for each card field of the output schema.
//!
//! An import is planned row by row before anything is written. A row updates
//! the entity its `entity_id` names, else the one its identifiers belong to,
//! else creates one. Blank cells leave values as they are; identifiers are
//! only ever added, and never taken from another entity.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ot_source::schema::{ExtType, ExtensionSchema, check_card};
use ot_store::cards::EntityWithCard;
use ot_store::{Card, RegistryEntity, RegistryIdentifier, registry::normalize_scheme};
use serde::Serialize;
use serde_json::{Map, Value};

/// A spreadsheet format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Xlsx,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "xlsx" => Some(Self::Xlsx),
            _ => None,
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Csv => "text/csv; charset=utf-8",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
        }
    }
}

/// Rows of text under a header.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sheet {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

const SHEET_NAME: &str = "Registry";

/// A cell's text for a JSON value: strings as they are, anything else as JSON.
fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The registry as a sheet, in `entities` order.
pub fn export(entities: &[(RegistryEntity, Option<Card>)], schema: &ExtensionSchema) -> Sheet {
    let schemes: BTreeSet<&str> = entities
        .iter()
        .flat_map(|(e, _)| e.identifiers.iter().map(|i| i.scheme.as_str()))
        .collect();
    let fields: BTreeSet<&str> = entities
        .iter()
        .flat_map(|(e, _)| e.fields.keys().map(String::as_str))
        .collect();
    let card_fields: Vec<&str> = schema
        .fields
        .iter()
        .filter(|f| f.builtin.is_none())
        .map(|f| f.key.as_str())
        .collect();
    let mut header: Vec<String> = ["entity_id", "name", "status"].map(String::from).to_vec();
    header.extend(schemes.iter().map(|s| format!("id:{s}")));
    header.extend(fields.iter().map(|k| format!("registry:{k}")));
    header.extend(card_fields.iter().map(|k| format!("card:{k}")));
    let rows = entities
        .iter()
        .map(|(e, card)| {
            let mut row = vec![
                e.id.clone(),
                e.name.clone().unwrap_or_default(),
                e.status.clone(),
            ];
            for s in &schemes {
                let values: Vec<&str> = e
                    .identifiers
                    .iter()
                    .filter(|i| i.scheme == *s)
                    .map(|i| i.value.as_str())
                    .collect();
                row.push(values.join("; "));
            }
            for k in &fields {
                row.push(e.fields.get(*k).map(text).unwrap_or_default());
            }
            for k in &card_fields {
                row.push(
                    card.as_ref()
                        .and_then(|c| c.values.get(*k))
                        .map(text)
                        .unwrap_or_default(),
                );
            }
            row
        })
        .collect();
    Sheet { header, rows }
}

/// Write a sheet as a file of `format`.
pub fn write(sheet: &Sheet, format: Format) -> Result<Vec<u8>, String> {
    match format {
        Format::Csv => {
            let mut w = csv::Writer::from_writer(Vec::new());
            w.write_record(&sheet.header).map_err(|e| e.to_string())?;
            for row in &sheet.rows {
                w.write_record(row).map_err(|e| e.to_string())?;
            }
            w.into_inner().map_err(|e| e.to_string())
        }
        Format::Xlsx => {
            use rust_xlsxwriter::{Format as Style, Workbook};
            let mut book = Workbook::new();
            let ws = book.add_worksheet();
            ws.set_name(SHEET_NAME).map_err(|e| e.to_string())?;
            let bold = Style::new().set_bold();
            for (c, h) in sheet.header.iter().enumerate() {
                ws.write_string_with_format(0, c as u16, h, &bold)
                    .map_err(|e| e.to_string())?;
            }
            for (r, row) in sheet.rows.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    // Everything as text: identifiers such as MMSIs must keep
                    // their digits exactly.
                    ws.write_string((r + 1) as u32, c as u16, v)
                        .map_err(|e| e.to_string())?;
                }
            }
            ws.set_freeze_panes(1, 0).map_err(|e| e.to_string())?;
            ws.autofit();
            book.save_to_buffer().map_err(|e| e.to_string())
        }
    }
}

/// Read a sheet from a file of `format` (XLSX: the first worksheet).
pub fn read(bytes: &[u8], format: Format) -> Result<Sheet, String> {
    let mut rows: Vec<Vec<String>> = match format {
        Format::Csv => {
            let text = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes))
                .map_err(|_| "the CSV is not UTF-8 text".to_owned())?;
            let mut r = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_reader(text.as_bytes());
            r.records()
                .map(|rec| {
                    rec.map(|rec| rec.iter().map(str::to_owned).collect())
                        .map_err(|e| format!("CSV: {e}"))
                })
                .collect::<Result<_, _>>()?
        }
        Format::Xlsx => {
            use calamine::{Data, Reader, Xlsx};
            let mut book: Xlsx<_> = Xlsx::new(std::io::Cursor::new(bytes.to_vec()))
                .map_err(|e| format!("not a readable XLSX file: {e}"))?;
            let range = book
                .worksheet_range_at(0)
                .ok_or_else(|| "the workbook has no worksheet".to_owned())?
                .map_err(|e| format!("XLSX: {e}"))?;
            range
                .rows()
                .map(|row| {
                    row.iter()
                        .map(|c| match c {
                            Data::Empty => String::new(),
                            Data::String(s) => s.clone(),
                            // Whole numbers typed into a spreadsheet (an MMSI
                            // entered as a number) without a trailing ".0".
                            Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => {
                                format!("{}", *f as i64)
                            }
                            other => other.to_string(),
                        })
                        .collect()
                })
                .collect()
        }
    };
    if rows.is_empty() {
        return Err("the sheet is empty".into());
    }
    let header = rows
        .remove(0)
        .into_iter()
        .map(|h| h.trim().to_owned())
        .collect();
    Ok(Sheet { header, rows })
}

/// What an import does with one row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Planned {
    /// The row's number in the sheet (the header is row 1).
    pub row: usize,
    /// `create`, `update`, `unchanged` or `error`.
    pub action: &'static str,
    pub entity_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identifiers_added: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub registry_fields: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub card_fields: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// The entity to write (with every identifier it should hold), when the
    /// registry part changes.
    #[serde(skip)]
    pub entity: Option<RegistryEntity>,
    /// The whole card to save, when it changes.
    #[serde(skip)]
    pub card: Option<Map<String, Value>>,
}

/// What a column holds.
enum Column {
    EntityId,
    Name,
    Status,
    Identifier(String),
    Registry(String),
    Card(String),
    Ignored,
}

fn columns(header: &[String], schema: &ExtensionSchema) -> Result<Vec<Column>, String> {
    let mut out = Vec::new();
    let mut unknown = Vec::new();
    for h in header {
        let lower = h.to_ascii_lowercase();
        let col = match lower.as_str() {
            "" => Column::Ignored,
            "entity_id" | "id" => Column::EntityId,
            "name" => Column::Name,
            "status" => Column::Status,
            _ => {
                if let Some(s) = lower.strip_prefix("id:") {
                    Column::Identifier(
                        normalize_scheme(s).map_err(|e| format!("column {h:?}: {e}"))?,
                    )
                } else if let Some(k) = h.strip_prefix("registry:").or(h.strip_prefix("REGISTRY:"))
                {
                    Column::Registry(k.trim().to_owned())
                } else if let Some(k) = h.strip_prefix("card:").or(h.strip_prefix("CARD:")) {
                    let k = k.trim();
                    match schema.field(k) {
                        Some(f) if f.builtin.is_none() => Column::Card(k.to_owned()),
                        _ => {
                            unknown.push(format!(
                                "{h} (no card field {k:?} in output schema version {})",
                                schema.version
                            ));
                            Column::Ignored
                        }
                    }
                } else {
                    unknown.push(h.clone());
                    Column::Ignored
                }
            }
        };
        out.push(col);
    }
    if !unknown.is_empty() {
        return Err(format!(
            "unknown columns: {} (expected entity_id, name, status, id:<scheme>, registry:<key>, card:<field>)",
            unknown.join(", ")
        ));
    }
    if !out
        .iter()
        .any(|c| matches!(c, Column::EntityId | Column::Identifier(_) | Column::Name))
    {
        return Err("the sheet needs an entity_id, name or id:<scheme> column".into());
    }
    Ok(out)
}

/// A registry cell's value: JSON when it is an object, an array, true, false
/// or null; text otherwise (so "0012" stays "0012").
fn registry_value(s: &str) -> Value {
    let t = s.trim();
    if (t.starts_with('{') || t.starts_with('[') || matches!(t, "true" | "false" | "null"))
        && let Ok(v) = serde_json::from_str(t)
    {
        return v;
    }
    Value::String(t.to_owned())
}

/// A card cell's value: JSON for position and JSON fields, text otherwise
/// (the schema coerces it to the field's type).
fn card_value(schema: &ExtensionSchema, key: &str, s: &str) -> Value {
    let t = s.trim();
    match schema.field(key).map(|f| f.kind) {
        Some(ExtType::Position | ExtType::Json) => {
            serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_owned()))
        }
        _ => Value::String(t.to_owned()),
    }
}

fn valid_entity_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// Plan an import. `entity` looks an entity (and its card) up by id;
/// `holder` finds the entity holding an identifier; `new_id` names new
/// entities.
pub fn plan(
    sheet: &Sheet,
    schema: &ExtensionSchema,
    entity: &mut dyn FnMut(&str) -> Option<EntityWithCard>,
    holder: &mut dyn FnMut(&str, &str) -> Option<String>,
    new_id: &mut dyn FnMut() -> String,
) -> Result<Vec<Planned>, String> {
    let cols = columns(&sheet.header, schema)?;
    let mut out = Vec::new();
    // Earlier rows' entities and identifiers: a sheet may name each once.
    let mut rows_of_entity: HashMap<String, usize> = HashMap::new();
    let mut rows_of_identifier: HashMap<String, usize> = HashMap::new();
    for (i, row) in sheet.rows.iter().enumerate() {
        let number = i + 2;
        let cell = |c: usize| row.get(c).map(|s| s.trim()).unwrap_or("");
        let (mut id, mut name, mut status) = (String::new(), None, None);
        let mut idents: Vec<(String, String)> = Vec::new();
        let mut fields = Map::new();
        let mut card = Map::new();
        for (c, col) in cols.iter().enumerate() {
            let v = cell(c);
            if v.is_empty() {
                continue;
            }
            match col {
                Column::EntityId => id = v.to_owned(),
                Column::Name => name = Some(v.to_owned()),
                Column::Status => status = Some(v.to_ascii_lowercase()),
                Column::Identifier(s) => idents.extend(
                    v.split(';')
                        .map(str::trim)
                        .filter(|x| !x.is_empty())
                        .map(|x| (s.clone(), x.to_owned())),
                ),
                Column::Registry(k) => {
                    fields.insert(k.clone(), registry_value(v));
                }
                Column::Card(k) => {
                    card.insert(k.clone(), card_value(schema, k, v));
                }
                Column::Ignored => {}
            }
        }
        if id.is_empty()
            && name.is_none()
            && idents.is_empty()
            && fields.is_empty()
            && card.is_empty()
        {
            continue;
        }
        let mut errors = Vec::new();
        if let Some(s) = &status
            && !matches!(s.as_str(), "active" | "retired")
        {
            errors.push(format!("status {s:?} must be active or retired"));
        }
        idents.sort();
        idents.dedup();
        // Who holds each identifier now.
        let holders: BTreeMap<String, String> = idents
            .iter()
            .filter_map(|(s, v)| holder(s, v).map(|h| (format!("{s}:{v}"), h)))
            .collect();
        let existing = if id.is_empty() {
            let owners: BTreeSet<&String> = holders.values().collect();
            match owners.len() {
                0 => None,
                1 => entity(owners.iter().next().expect("one")),
                _ => {
                    errors.push(format!(
                        "its identifiers belong to different entities: {}",
                        holders
                            .iter()
                            .map(|(i, h)| format!("{i} ({h})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    None
                }
            }
        } else if !valid_entity_id(&id) {
            errors.push(format!(
                "entity_id {id:?} must be letters, digits, '-', '_', '.' or ':' (at most 64)"
            ));
            None
        } else {
            entity(&id)
        };
        let target = existing
            .as_ref()
            .map(|(e, _)| e.id.clone())
            .or_else(|| (!id.is_empty()).then(|| id.clone()))
            .unwrap_or_else(&mut *new_id);
        for (ident, h) in &holders {
            if *h != target {
                errors.push(format!("{ident} already belongs to entity {h}"));
            }
        }
        if let Some(first) = rows_of_entity.insert(target.clone(), number) {
            errors.push(format!("entity {target} is also in row {first}"));
        }
        for (s, v) in &idents {
            if let Some(first) = rows_of_identifier.insert(format!("{s}:{v}"), number) {
                errors.push(format!("{s}:{v} is also in row {first}"));
            }
        }

        // The entity after the row.
        let (before, before_card) = match &existing {
            Some((e, c)) => (Some(e.clone()), c.as_ref().map(|c| c.values.clone())),
            None => (None, None),
        };
        let mut after = before.clone().unwrap_or(RegistryEntity {
            id: target.clone(),
            name: None,
            status: "active".into(),
            fields: Map::new(),
            source: Some("sheet".into()),
            identifiers: Vec::new(),
        });
        if let Some(n) = &name {
            after.name = Some(n.clone());
        }
        if let Some(s) = &status {
            after.status = s.clone();
        }
        let mut registry_fields = Vec::new();
        for (k, v) in fields {
            if after.fields.get(&k) != Some(&v) {
                registry_fields.push(k.clone());
                after.fields.insert(k, v);
            }
        }
        let mut identifiers_added = Vec::new();
        for (s, v) in &idents {
            if !after
                .identifiers
                .iter()
                .any(|i| i.scheme == *s && i.value == *v)
            {
                identifiers_added.push(format!("{s}:{v}"));
                after.identifiers.push(RegistryIdentifier {
                    scheme: s.clone(),
                    value: v.clone(),
                    expected_name: after.name.clone(),
                    source: Some("sheet".into()),
                });
            }
        }
        let mut merged = before_card.clone().unwrap_or_default();
        let mut card_fields = Vec::new();
        for (k, v) in card {
            merged.insert(k, v);
        }
        let merged = match check_card(schema, &merged) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("card: {e}"));
                Map::new()
            }
        };
        for (k, v) in &merged {
            if before_card.as_ref().and_then(|c| c.get(k)) != Some(v) {
                card_fields.push(k.clone());
            }
        }
        let entity_changes = before.as_ref() != Some(&after);
        let action = if !errors.is_empty() {
            "error"
        } else if before.is_none() {
            "create"
        } else if entity_changes || !card_fields.is_empty() {
            "update"
        } else {
            "unchanged"
        };
        out.push(Planned {
            row: number,
            action,
            entity_id: target,
            name: after.name.clone(),
            identifiers_added,
            registry_fields,
            card_fields: card_fields.clone(),
            errors,
            entity: (action != "error" && (entity_changes || before.is_none())).then_some(after),
            card: (action != "error" && !card_fields.is_empty()).then_some(merged),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> ExtensionSchema {
        ExtensionSchema {
            version: 2,
            fields: serde_json::from_value(json!([
                {"key": "contact_phone", "type": "string"},
                {"key": "crew", "type": "integer"},
                {"key": "state", "type": "string", "builtin": "state"}
            ]))
            .unwrap(),
        }
    }

    fn ident(scheme: &str, value: &str) -> RegistryIdentifier {
        RegistryIdentifier {
            scheme: scheme.into(),
            value: value.into(),
            expected_name: None,
            source: None,
        }
    }

    fn ted() -> (RegistryEntity, Option<Card>) {
        let mut fields = Map::new();
        fields.insert("flag".into(), json!("US"));
        (
            RegistryEntity {
                id: "ent-ted".into(),
                name: Some("TED STEVENS".into()),
                status: "active".into(),
                fields,
                source: None,
                identifiers: vec![ident("mmsi", "338000001"), ident("imo", "0012345")],
            },
            Some(Card {
                entity_id: "ent-ted".into(),
                values: serde_json::from_value(json!({"contact_phone": "+1 555 0100"})).unwrap(),
                schema_version: 2,
                updated_at_ms: 0,
                decision_id: None,
            }),
        )
    }

    #[test]
    fn export_and_read_back_in_both_formats() {
        let sheet = export(&[ted()], &schema());
        assert_eq!(
            sheet.header,
            [
                "entity_id",
                "name",
                "status",
                "id:imo",
                "id:mmsi",
                "registry:flag",
                "card:contact_phone",
                "card:crew"
            ]
        );
        assert_eq!(
            sheet.rows[0],
            [
                "ent-ted",
                "TED STEVENS",
                "active",
                "0012345",
                "338000001",
                "US",
                "+1 555 0100",
                ""
            ]
        );
        for format in [Format::Csv, Format::Xlsx] {
            let bytes = write(&sheet, format).unwrap();
            let back = read(&bytes, format).unwrap();
            assert_eq!(back.header, sheet.header, "{format:?}");
            // Leading zeros survive; trailing empty cells may be cut.
            assert_eq!(back.rows[0][..7], sheet.rows[0][..7], "{format:?}");
        }
    }

    fn run(sheet: &Sheet) -> Vec<Planned> {
        let (e, c) = ted();
        let mut n = 0;
        plan(
            sheet,
            &schema(),
            &mut |id| (id == "ent-ted").then(|| (e.clone(), c.clone())),
            &mut |s, v| {
                (e.identifiers.iter().any(|i| i.scheme == s && i.value == v))
                    .then(|| "ent-ted".into())
            },
            &mut || {
                n += 1;
                format!("ent-new{n}")
            },
        )
        .unwrap()
    }

    fn sheet(header: &[&str], rows: &[&[&str]]) -> Sheet {
        Sheet {
            header: header.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
        }
    }

    #[test]
    fn rows_update_by_identifier_create_new_and_refuse_conflicts() {
        let s = sheet(
            &["name", "id:mmsi", "id:icao", "registry:flag", "card:crew"],
            &[
                // Found by its MMSI: a new ICAO, the crew count, the same flag.
                &["", "338000001", "a1b2c3", "US", "12"],
                // New.
                &["NEW ONE", "366999999", "", "", ""],
                // Claims TED STEVENS's MMSI for a new entity... by entity_id? No:
                // same MMSI as row 2, and a crew count that is not a number.
                &["BAD", "338000001", "", "", "many"],
                &["", "", "", "", ""],
            ],
        );
        let p = run(&s);
        assert_eq!(p.len(), 3, "blank rows are skipped");
        assert_eq!(
            (p[0].action, p[0].entity_id.as_str()),
            ("update", "ent-ted")
        );
        assert_eq!(p[0].identifiers_added, ["icao:a1b2c3"]);
        assert!(p[0].registry_fields.is_empty(), "flag unchanged");
        assert_eq!(p[0].card_fields, ["crew"]);
        let card = p[0].card.as_ref().unwrap();
        assert_eq!(card["crew"], json!(12));
        assert_eq!(
            card["contact_phone"],
            json!("+1 555 0100"),
            "blank cells keep values"
        );
        assert_eq!(p[0].entity.as_ref().unwrap().identifiers.len(), 3);

        assert_eq!(
            (p[1].action, p[1].entity_id.as_str()),
            ("create", "ent-new1")
        );
        assert_eq!(
            p[1].entity.as_ref().unwrap().identifiers[0].label(),
            "mmsi:366999999"
        );

        assert_eq!(p[2].action, "error");
        let errors = p[2].errors.join(" | ");
        assert!(errors.contains("also in row 2"), "{errors}");
        assert!(errors.contains("card:"), "{errors}");
        assert!(p[2].entity.is_none() && p[2].card.is_none());
    }

    #[test]
    fn an_identifier_is_never_taken_from_another_entity() {
        let s = sheet(&["entity_id", "id:mmsi"], &[&["ent-other", "338000001"]]);
        let p = run(&s);
        assert_eq!(p[0].action, "error");
        assert!(p[0].errors[0].contains("already belongs to entity ent-ted"));
    }

    #[test]
    fn unknown_columns_and_unchanged_rows() {
        let e = plan(
            &sheet(&["name", "colour"], &[&["X", "red"]]),
            &schema(),
            &mut |_| None,
            &mut |_, _| None,
            &mut || "ent-x".into(),
        )
        .unwrap_err();
        assert!(e.contains("colour"), "{e}");
        let e = plan(
            &sheet(&["name", "card:state"], &[&["X", "live"]]),
            &schema(),
            &mut |_| None,
            &mut |_, _| None,
            &mut || "ent-x".into(),
        )
        .unwrap_err();
        assert!(
            e.contains("card:state"),
            "a built-in is not a card field: {e}"
        );
        let p = run(&sheet(
            &["entity_id", "name"],
            &[&["ent-ted", "TED STEVENS"]],
        ));
        assert_eq!(p[0].action, "unchanged");
        assert!(p[0].entity.is_none() && p[0].card.is_none());
    }
}
