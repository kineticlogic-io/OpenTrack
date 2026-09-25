//! Captured probe frames, kept per source for mapping preview and plugin
//! conformance tests. Capped per source (oldest dropped).

use rusqlite::params;

use crate::sqlite::{Db, Result, now_ms};

/// Default number of frames kept per source.
pub const DEFAULT_SAMPLE_CAP: usize = 200;

impl Db {
    /// Replace a source's stored samples with `frames`, keeping at most `cap`.
    pub fn store_probe_samples(
        &mut self,
        source_id: &str,
        frames: &[Vec<u8>],
        cap: usize,
    ) -> Result<usize> {
        self.write(|tx| {
            tx.execute("DELETE FROM probe_samples WHERE source_id = ?1", [source_id])?;
            let now = now_ms();
            let mut n = 0;
            for f in frames.iter().take(cap) {
                tx.execute(
                    "INSERT INTO probe_samples (source_id, captured_at_ms, frame) VALUES (?1, ?2, ?3)",
                    params![source_id, now, f],
                )?;
                n += 1;
            }
            Ok(n)
        })
    }

    /// Stored samples of a source, oldest first.
    pub fn probe_samples(&self, source_id: &str, limit: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut stmt = self.connection().prepare(
            "SELECT captured_at_ms, frame FROM probe_samples WHERE source_id = ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![source_id, limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_replaced_and_capped() {
        let mut db = Db::open_in_memory().unwrap();
        let frames: Vec<Vec<u8>> = (0..5).map(|i| format!("f{i}").into_bytes()).collect();
        assert_eq!(db.store_probe_samples("s", &frames, 3).unwrap(), 3);
        assert_eq!(db.probe_samples("s", 10).unwrap().len(), 3);
        db.store_probe_samples("s", &frames[4..], 3).unwrap();
        let got = db.probe_samples("s", 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, b"f4");
        assert!(db.probe_samples("other", 10).unwrap().is_empty());
    }
}
