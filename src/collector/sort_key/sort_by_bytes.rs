use columnar::BytesColumn;

use crate::collector::sort_key::NaturalComparator;
use crate::collector::{SegmentSortKeyComputer, SortKeyComputer};
use crate::termdict::TermOrdinal;
use crate::{DocId, Score};

/// Sort by the first value of a bytes column.
///
/// If the field is multivalued, only the first value is considered.
///
/// Documents that do not have this value are still considered.
/// Their sort key will simply be `None`.
#[derive(Debug, Clone)]
pub struct SortByBytes {
    column_name: String,
}

impl SortByBytes {
    /// Creates a new sort by bytes sort key computer.
    pub fn for_field(column_name: impl ToString) -> Self {
        SortByBytes {
            column_name: column_name.to_string(),
        }
    }
}

impl SortKeyComputer for SortByBytes {
    type SortKey = Option<Vec<u8>>;
    type Child = ByBytesColumnSegmentSortKeyComputer;
    type Comparator = NaturalComparator;

    fn segment_sort_key_computer(
        &self,
        segment_reader: &crate::SegmentReader,
    ) -> crate::Result<Self::Child> {
        let bytes_column_opt = segment_reader.fast_fields().bytes(&self.column_name)?;
        Ok(ByBytesColumnSegmentSortKeyComputer { bytes_column_opt })
    }
}

/// Segment-level sort key computer for bytes columns.
pub struct ByBytesColumnSegmentSortKeyComputer {
    bytes_column_opt: Option<BytesColumn>,
}

impl SegmentSortKeyComputer for ByBytesColumnSegmentSortKeyComputer {
    type SortKey = Option<Vec<u8>>;
    type SegmentSortKey = Option<TermOrdinal>;
    type SegmentComparator = NaturalComparator;

    #[inline(always)]
    fn segment_sort_key(&mut self, doc: DocId, _score: Score) -> Option<TermOrdinal> {
        let bytes_column = self.bytes_column_opt.as_ref()?;
        bytes_column.ords().first(doc)
    }

    fn convert_segment_sort_key(&self, term_ord_opt: Option<TermOrdinal>) -> Option<Vec<u8>> {
        let term_ord = term_ord_opt?;
        let bytes_column = self.bytes_column_opt.as_ref()?;
        let mut bytes = Vec::new();
        bytes_column
            .dictionary()
            .ord_to_term(term_ord, &mut bytes)
            .ok()?;
        Some(bytes)
    }

    fn convert_segment_sort_keys(
        &self,
        term_ords: Vec<Option<TermOrdinal>>,
    ) -> Vec<Option<Vec<u8>>> {
        match self.bytes_column_opt.as_ref() {
            Some(bytes_column) => term_ords_to_terms(bytes_column, &term_ords),
            None => vec![None; term_ords.len()],
        }
    }
}

/// Looks up the terms of `term_ords` in `bytes_column`'s dictionary, in order.
///
/// The ordinals are sorted and deduplicated first so that the dictionary is walked
/// once, decompressing each block at most once, instead of once per lookup. An ordinal
/// that is `None` or cannot be resolved yields `None`.
pub(crate) fn term_ords_to_terms(
    bytes_column: &BytesColumn,
    term_ords: &[Option<TermOrdinal>],
) -> Vec<Option<Vec<u8>>> {
    let mut sorted_ords: Vec<TermOrdinal> = term_ords.iter().flatten().copied().collect();
    sorted_ords.sort_unstable();
    sorted_ords.dedup();
    let mut terms: Vec<Vec<u8>> = Vec::with_capacity(sorted_ords.len());
    // On a missing ordinal or an I/O error the walk stops early: `terms` then holds the
    // prefix that was resolved, and every later ordinal maps to `None` below.
    let _ = bytes_column
        .dictionary()
        .sorted_ords_to_term_cb(sorted_ords.iter().copied(), |term| {
            terms.push(term.to_vec());
            Ok(())
        });
    term_ords
        .iter()
        .map(|term_ord| {
            let position = sorted_ords.binary_search(&(*term_ord)?).ok()?;
            terms.get(position).cloned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::SortByBytes;
    use crate::collector::TopDocs;
    use crate::query::AllQuery;
    use crate::schema::{BytesOptions, Schema, FAST, INDEXED};
    use crate::{Index, IndexWriter, Order, TantivyDocument};

    #[test]
    fn test_sort_by_bytes_asc() -> crate::Result<()> {
        let mut schema_builder = Schema::builder();
        let bytes_field = schema_builder
            .add_bytes_field("data", BytesOptions::default().set_fast().set_indexed());
        let id_field = schema_builder.add_u64_field("id", FAST | INDEXED);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let mut index_writer: IndexWriter = index.writer_for_tests()?;

        // Insert documents with byte values in non-sorted order
        let test_data: Vec<(u64, Vec<u8>)> = vec![
            (1, vec![0x02, 0x00]),
            (2, vec![0x00, 0x10]),
            (3, vec![0x01, 0x00]),
            (4, vec![0x00, 0x20]),
        ];

        for (id, bytes) in &test_data {
            let mut doc = TantivyDocument::new();
            doc.add_u64(id_field, *id);
            doc.add_bytes(bytes_field, bytes);
            index_writer.add_document(doc)?;
        }
        index_writer.commit()?;

        let reader = index.reader()?;
        let searcher = reader.searcher();

        // Sort ascending by bytes
        let top_docs =
            TopDocs::with_limit(10).order_by((SortByBytes::for_field("data"), Order::Asc));
        let results: Vec<(Option<Vec<u8>>, _)> = searcher.search(&AllQuery, &top_docs)?;

        // Expected order: [0x00,0x10], [0x00,0x20], [0x01,0x00], [0x02,0x00]
        let sorted_bytes: Vec<Option<Vec<u8>>> = results.into_iter().map(|(b, _)| b).collect();
        assert_eq!(
            sorted_bytes,
            vec![
                Some(vec![0x00, 0x10]),
                Some(vec![0x00, 0x20]),
                Some(vec![0x01, 0x00]),
                Some(vec![0x02, 0x00]),
            ]
        );

        Ok(())
    }

    #[test]
    fn test_sort_by_bytes_desc() -> crate::Result<()> {
        let mut schema_builder = Schema::builder();
        let bytes_field = schema_builder
            .add_bytes_field("data", BytesOptions::default().set_fast().set_indexed());
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let mut index_writer: IndexWriter = index.writer_for_tests()?;

        let test_data: Vec<Vec<u8>> = vec![vec![0x00, 0x10], vec![0x02, 0x00], vec![0x01, 0x00]];

        for bytes in &test_data {
            let mut doc = TantivyDocument::new();
            doc.add_bytes(bytes_field, bytes);
            index_writer.add_document(doc)?;
        }
        index_writer.commit()?;

        let reader = index.reader()?;
        let searcher = reader.searcher();

        // Sort descending by bytes
        let top_docs =
            TopDocs::with_limit(10).order_by((SortByBytes::for_field("data"), Order::Desc));
        let results: Vec<(Option<Vec<u8>>, _)> = searcher.search(&AllQuery, &top_docs)?;

        // Expected order (descending): [0x02,0x00], [0x01,0x00], [0x00,0x10]
        let sorted_bytes: Vec<Option<Vec<u8>>> = results.into_iter().map(|(b, _)| b).collect();
        assert_eq!(
            sorted_bytes,
            vec![
                Some(vec![0x02, 0x00]),
                Some(vec![0x01, 0x00]),
                Some(vec![0x00, 0x10]),
            ]
        );

        Ok(())
    }
}
