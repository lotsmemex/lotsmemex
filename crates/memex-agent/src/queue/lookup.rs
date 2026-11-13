use super::*;

impl Queue {
    /// Per-origin politeness for explicit metadata lookups, sharing the
    /// capture `capture_origins` backoff table (including server Retry-After
    /// recorded by `defer_capture_origin`). Returns the instant to wait for, or
    /// reserves the origin for `spacing` seconds and returns `None`.
    pub(crate) fn reserve_lookup_origin(
        &mut self,
        origin: &str,
        now: i64,
        spacing: i64,
    ) -> Result<Option<i64>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next: Option<i64> = tx
            .query_row(
                "SELECT next_fetch_at FROM capture_origins WHERE origin=?1",
                [origin],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(until) = next.filter(|until| *until > now) {
            return Ok(Some(until));
        }
        tx.execute("INSERT INTO capture_origins(origin,next_fetch_at) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET next_fetch_at=MAX(next_fetch_at,excluded.next_fetch_at)", params![origin, now.saturating_add(spacing)])?;
        tx.commit()?;
        self.secure()?;
        Ok(None)
    }
}
