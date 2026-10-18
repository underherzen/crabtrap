//! Буфер строк таблицы: кольцо (`VecDeque`) + индекс `id → номер`.
//!
//! Номер (seq) строки растёт монотонно и не меняется при вытеснении старых
//! строк: позиция = `seq - base`. Поэтому вытеснение — `pop_front` за O(1)
//! без перестройки индекса, а поиск строки по id — O(1). При 10k строк и
//! непрерывном вытеснении это то, что держит UI-поток свободным.

use std::collections::{HashMap, VecDeque};
use std::ops::Index;

use domain::value_objects::ExchangeId;

use super::store::TrafficRow;

#[derive(Debug, Default)]
pub struct RowBuffer {
    rows: VecDeque<TrafficRow>,
    /// Номер `rows[0]`.
    base: u64,
    index: HashMap<ExchangeId, u64>,
}

impl RowBuffer {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &TrafficRow> + ExactSizeIterator {
        self.rows.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut TrafficRow> {
        self.rows.iter_mut()
    }

    /// Строки с их номерами.
    pub fn iter_seq(&self) -> impl Iterator<Item = (u64, &TrafficRow)> {
        let base = self.base;
        self.rows
            .iter()
            .enumerate()
            .map(move |(ix, row)| (base + ix as u64, row))
    }

    /// Номер первой строки.
    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn contains(&self, id: &ExchangeId) -> bool {
        self.index.contains_key(id)
    }

    /// Добавить строку в конец; её номер.
    pub fn push(&mut self, row: TrafficRow) -> u64 {
        let seq = self.base + self.rows.len() as u64;
        self.index.insert(row.summary.id, seq);
        self.rows.push_back(row);
        seq
    }

    pub fn seq_of(&self, id: &ExchangeId) -> Option<u64> {
        self.index.get(id).copied()
    }

    /// Позиция строки по номеру (None — вытеснена или ещё не было).
    pub fn position(&self, seq: u64) -> Option<usize> {
        let ix = usize::try_from(seq.checked_sub(self.base)?).ok()?;
        (ix < self.rows.len()).then_some(ix)
    }

    pub fn by_seq(&self, seq: u64) -> Option<&TrafficRow> {
        self.rows.get(self.position(seq)?)
    }

    pub fn by_seq_mut(&mut self, seq: u64) -> Option<&mut TrafficRow> {
        let ix = self.position(seq)?;
        self.rows.get_mut(ix)
    }

    pub fn get(&self, id: &ExchangeId) -> Option<&TrafficRow> {
        self.by_seq(self.seq_of(id)?)
    }

    pub fn by_position(&self, ix: usize) -> Option<&TrafficRow> {
        self.rows.get(ix)
    }

    /// Вытеснить самую старую строку.
    pub fn pop_front(&mut self) -> Option<TrafficRow> {
        let row = self.rows.pop_front()?;
        if self.index.get(&row.summary.id) == Some(&self.base) {
            self.index.remove(&row.summary.id);
        }
        self.base += 1;
        Some(row)
    }

    /// Очистить (номера продолжают расти — старые seq не оживут).
    pub fn clear(&mut self) {
        self.base += self.rows.len() as u64;
        self.rows.clear();
        self.index.clear();
    }
}

impl Index<usize> for RowBuffer {
    type Output = TrafficRow;

    fn index(&self, ix: usize) -> &TrafficRow {
        &self.rows[ix]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::traffic::store::test_row;

    #[test]
    fn eviction_keeps_lookups_valid_without_reindexing() {
        let mut buf = RowBuffer::default();
        let rows: Vec<_> = (0..5)
            .map(|i| test_row("a.test", &format!("/{i}")))
            .collect();
        let ids: Vec<_> = rows.iter().map(|r| r.summary.id).collect();
        let seqs: Vec<_> = rows.into_iter().map(|r| buf.push(r)).collect();
        assert_eq!(seqs, [0, 1, 2, 3, 4]);
        buf.pop_front();
        buf.pop_front();
        assert_eq!(buf.len(), 3);
        assert!(buf.get(&ids[0]).is_none());
        assert_eq!(
            buf.get(&ids[3]).map(|r| r.summary.path.as_str()),
            Some("/3")
        );
        assert_eq!(buf.position(3), Some(1));
        assert_eq!(buf.position(1), None, "evicted seq");
        assert_eq!(buf[0].summary.path, "/2");
    }

    #[test]
    fn clear_never_revives_old_seqs() {
        let mut buf = RowBuffer::default();
        buf.push(test_row("a.test", "/0"));
        buf.clear();
        let seq = buf.push(test_row("a.test", "/1"));
        assert_eq!(seq, 1);
        assert_eq!(buf.position(0), None);
        assert_eq!(buf.position(1), Some(0));
    }
}
