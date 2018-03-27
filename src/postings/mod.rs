/*!
Postings module (also called inverted index)
*/

/// Postings module
///
/// Postings, also called inverted lists, is the key datastructure
/// to full-text search.

mod postings;
mod recorder;
mod serializer;
mod postings_writer;
mod term_info;
mod segment_postings;

use self::recorder::{NothingRecorder, Recorder, TFAndPositionRecorder, TermFrequencyRecorder};
pub use self::serializer::{FieldSerializer, InvertedIndexSerializer};
pub(crate) use self::postings_writer::MultiFieldPostingsWriter;

pub use self::term_info::TermInfo;
pub use self::postings::Postings;

pub use self::segment_postings::{BlockSegmentPostings, SegmentPostings};

pub use common::HasLen;

pub(crate) type UnorderedTermId = u64;

#[allow(enum_variant_names)]
pub(crate) enum FreqReadingOption {
    NoFreq,
    SkipFreq,
    ReadFreq,
}

#[cfg(test)]
pub mod tests {

    use super::*;
    use docset::{DocSet, SkipResult};
    use DocId;
    use Score;
    use query::Intersection;
    use query::Scorer;
    use schema::{Document, SchemaBuilder, Term, INT_INDEXED, STRING, TEXT};
    use core::SegmentComponent;
    use indexer::SegmentWriter;
    use core::SegmentReader;
    use core::Index;
    use schema::IndexRecordOption;
    use std::iter;
    use datastruct::stacker::Heap;
    use schema::Field;
    use test::{self, Bencher};
    use indexer::operation::AddOperation;
    use tests;
    use rand::{Rng, SeedableRng, XorShiftRng};
    use fieldnorm::FieldNormReader;

    #[test]
    pub fn test_position_write() {
        let mut schema_builder = SchemaBuilder::default();
        let text_field = schema_builder.add_text_field("text", TEXT);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let mut segment = index.new_segment();
        let mut posting_serializer = InvertedIndexSerializer::open(&mut segment).unwrap();
        {
            let mut field_serializer = posting_serializer.new_field(text_field, 120 * 4).unwrap();
            field_serializer.new_term("abc".as_bytes()).unwrap();
            for doc_id in 0u32..120u32 {
                let delta_positions = vec![1, 2, 3, 2];
                field_serializer
                    .write_doc(doc_id, 4, &delta_positions)
                    .unwrap();
            }
            field_serializer.close_term().unwrap();
        }
        posting_serializer.close().unwrap();
        let read = segment.open_read(SegmentComponent::POSITIONS).unwrap();
        assert!(read.len() <= 140);
    }

    #[test]
    pub fn test_skip_positions() {
        let mut schema_builder = SchemaBuilder::new();
        let title = schema_builder.add_text_field("title", TEXT);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let mut index_writer = index.writer_with_num_threads(1, 30_000_000).unwrap();
        index_writer.add_document(doc!(title => r#"abc abc abc"#));
        index_writer.add_document(doc!(title => r#"abc be be be be abc"#));
        for _ in 0..1_000 {
            index_writer.add_document(doc!(title => r#"abc abc abc"#));
        }
        index_writer.add_document(doc!(title => r#"abc be be be be abc"#));
        index_writer.commit().unwrap();
        index.load_searchers().unwrap();

        let searcher = index.searcher();
        let inverted_index = searcher.segment_reader(0u32).inverted_index(title);
        let term = Term::from_field_text(title, "abc");

        let mut positions = Vec::new();

        {
            let mut postings = inverted_index
                .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)
                .unwrap();
            postings.advance();
            postings.positions(&mut positions);
            assert_eq!(&[0, 1, 2], &positions[..]);
            postings.positions(&mut positions);
            assert_eq!(&[0, 1, 2], &positions[..]);
            postings.advance();
            postings.positions(&mut positions);
            assert_eq!(&[0, 5], &positions[..]);
        }
        {
            let mut postings = inverted_index
                .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)
                .unwrap();
            postings.advance();
            postings.advance();
            postings.positions(&mut positions);
            assert_eq!(&[0, 5], &positions[..]);
        }
        {

            let mut postings = inverted_index
                .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)
                .unwrap();
            assert_eq!(postings.skip_next(1), SkipResult::Reached);
            assert_eq!(postings.doc(), 1);
            postings.positions(&mut positions);
            assert_eq!(&[0, 5], &positions[..]);
        }
        {
            let mut postings = inverted_index
                .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)
                .unwrap();
            assert_eq!(postings.skip_next(1002), SkipResult::Reached);
            assert_eq!(postings.doc(), 1002);
            postings.positions(&mut positions);
            assert_eq!(&[0, 5], &positions[..]);
        }
        {
            let mut postings = inverted_index
                .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)
                .unwrap();
            assert_eq!(postings.skip_next(100), SkipResult::Reached);
            assert_eq!(postings.skip_next(1002), SkipResult::Reached);
            assert_eq!(postings.doc(), 1002);
            postings.positions(&mut positions);
            assert_eq!(&[0, 5], &positions[..]);
        }
    }

    #[test]
    pub fn test_position_and_fieldnorm1() {
        let mut positions = Vec::new();
        let mut schema_builder = SchemaBuilder::default();
        let text_field = schema_builder.add_text_field("text", TEXT);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema.clone());
        let segment = index.new_segment();

        let heap = Heap::with_capacity(10_000_000);
        {
            let mut segment_writer =
                SegmentWriter::for_segment(&heap, 18, segment.clone(), &schema).unwrap();
            {
                let mut doc = Document::default();
                // checking that position works if the field has two values
                doc.add_text(text_field, "a b a c a d a a.");
                doc.add_text(text_field, "d d d d a");
                let op = AddOperation {
                    opstamp: 0u64,
                    document: doc,
                };
                segment_writer.add_document(op, &schema).unwrap();
            }
            {
                let mut doc = Document::default();
                doc.add_text(text_field, "b a");
                let op = AddOperation {
                    opstamp: 1u64,
                    document: doc,
                };
                segment_writer.add_document(op, &schema).unwrap();
            }
            for i in 2..1000 {
                let mut doc = Document::default();
                let mut text = iter::repeat("e ").take(i).collect::<String>();
                text.push_str(" a");
                doc.add_text(text_field, &text);
                let op = AddOperation {
                    opstamp: 2u64,
                    document: doc,
                };
                segment_writer.add_document(op, &schema).unwrap();
            }
            segment_writer.finalize().unwrap();
        }
        {
            let segment_reader = SegmentReader::open(&segment).unwrap();
            {
                let fieldnorm_reader = segment_reader.get_fieldnorms_reader(text_field) ;
                assert_eq!(fieldnorm_reader.fieldnorm(0), 8 + 5);
                assert_eq!(fieldnorm_reader.fieldnorm(1), 2);
                for i in 2..1000 {
                    assert_eq!(
                        fieldnorm_reader.fieldnorm_id(i),
                        FieldNormReader::fieldnorm_to_id(i + 1) );
                }
            }
            {
                let term_a = Term::from_field_text(text_field, "abcdef");
                assert!(
                    segment_reader
                        .inverted_index(term_a.field())
                        .read_postings(&term_a, IndexRecordOption::WithFreqsAndPositions)
                        .is_none()
                );
            }
            {
                let term_a = Term::from_field_text(text_field, "a");
                let mut postings_a = segment_reader
                    .inverted_index(term_a.field())
                    .read_postings(&term_a, IndexRecordOption::WithFreqsAndPositions)
                    .unwrap();
                assert_eq!(postings_a.len(), 1000);
                assert!(postings_a.advance());
                assert_eq!(postings_a.doc(), 0);
                assert_eq!(postings_a.term_freq(), 6);
                postings_a.positions(&mut positions);
                assert_eq!(&positions[..], [0, 2, 4, 6, 7, 13]);
                assert!(postings_a.advance());
                assert_eq!(postings_a.doc(), 1u32);
                assert_eq!(postings_a.term_freq(), 1);
                for i in 2u32..1000u32 {
                    assert!(postings_a.advance());
                    assert_eq!(postings_a.term_freq(), 1);
                    postings_a.positions(&mut positions);
                    assert_eq!(&positions[..], [i]);
                    assert_eq!(postings_a.doc(), i);
                }
                assert!(!postings_a.advance());
            }
            {
                let term_e = Term::from_field_text(text_field, "e");
                let mut postings_e = segment_reader
                    .inverted_index(term_e.field())
                    .read_postings(&term_e, IndexRecordOption::WithFreqsAndPositions)
                    .unwrap();
                assert_eq!(postings_e.len(), 1000 - 2);
                for i in 2u32..1000u32 {
                    assert!(postings_e.advance());
                    assert_eq!(postings_e.term_freq(), i);
                    postings_e.positions(&mut positions);
                    assert_eq!(positions.len(), i as usize);
                    for j in 0..positions.len() {
                        assert_eq!(positions[j], (j as u32));
                    }
                    assert_eq!(postings_e.doc(), i);
                }
                assert!(!postings_e.advance());
            }
        }
    }

    #[test]
    pub fn test_position_and_fieldnorm2() {
        let mut positions: Vec<u32> = Vec::new();
        let mut schema_builder = SchemaBuilder::default();
        let text_field = schema_builder.add_text_field("text", TEXT);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        {
            let mut index_writer = index.writer_with_num_threads(1, 40_000_000).unwrap();
            {
                let mut doc = Document::default();
                doc.add_text(text_field, "g b b d c g c");
                index_writer.add_document(doc);
            }
            {
                let mut doc = Document::default();
                doc.add_text(text_field, "g a b b a d c g c");
                index_writer.add_document(doc);
            }
            assert!(index_writer.commit().is_ok());
        }
        index.load_searchers().unwrap();
        let term_a = Term::from_field_text(text_field, "a");
        let searcher = index.searcher();
        let segment_reader = searcher.segment_reader(0);
        let mut postings = segment_reader
            .inverted_index(text_field)
            .read_postings(&term_a, IndexRecordOption::WithFreqsAndPositions)
            .unwrap();
        assert!(postings.advance());
        assert_eq!(postings.doc(), 1u32);
        postings.positions(&mut positions);
        assert_eq!(&positions[..], &[1u32, 4]);
    }

    #[test]
    fn test_skip_next() {
        let term_0 = Term::from_field_u64(Field(0), 0);
        let term_1 = Term::from_field_u64(Field(0), 1);
        let term_2 = Term::from_field_u64(Field(0), 2);

        let num_docs = 300u32;

        let index = {
            let mut schema_builder = SchemaBuilder::default();
            let value_field = schema_builder.add_u64_field("value", INT_INDEXED);
            let schema = schema_builder.build();

            let index = Index::create_in_ram(schema);
            {
                let mut index_writer = index.writer_with_num_threads(1, 40_000_000).unwrap();
                for i in 0..num_docs {
                    let mut doc = Document::default();
                    doc.add_u64(value_field, 2);
                    doc.add_u64(value_field, (i % 2) as u64);

                    index_writer.add_document(doc);
                }
                assert!(index_writer.commit().is_ok());
            }
            index.load_searchers().unwrap();

            index
        };
        let searcher = index.searcher();
        let segment_reader = searcher.segment_reader(0);

        // check that the basic usage works
        for i in 0..num_docs - 1 {
            for j in i + 1..num_docs {
                let mut segment_postings = segment_reader
                    .inverted_index(term_2.field())
                    .read_postings(&term_2, IndexRecordOption::Basic)
                    .unwrap();

                assert_eq!(segment_postings.skip_next(i), SkipResult::Reached);
                assert_eq!(segment_postings.doc(), i);

                assert_eq!(segment_postings.skip_next(j), SkipResult::Reached);
                assert_eq!(segment_postings.doc(), j);
            }
        }

        {
            let mut segment_postings = segment_reader
                .inverted_index(term_2.field())
                .read_postings(&term_2, IndexRecordOption::Basic)
                .unwrap();

            // check that `skip_next` advances the iterator
            assert!(segment_postings.advance());
            assert_eq!(segment_postings.doc(), 0);

            assert_eq!(segment_postings.skip_next(1), SkipResult::Reached);
            assert_eq!(segment_postings.doc(), 1);

            assert_eq!(segment_postings.skip_next(1), SkipResult::OverStep);
            assert_eq!(segment_postings.doc(), 2);

            // check that going beyond the end is handled
            assert_eq!(segment_postings.skip_next(num_docs), SkipResult::End);
        }

        // check that filtering works
        {
            let mut segment_postings = segment_reader
                .inverted_index(term_0.field())
                .read_postings(&term_0, IndexRecordOption::Basic)
                .unwrap();

            for i in 0..num_docs / 2 {
                assert_eq!(segment_postings.skip_next(i * 2), SkipResult::Reached);
                assert_eq!(segment_postings.doc(), i * 2);
            }

            let mut segment_postings = segment_reader
                .inverted_index(term_0.field())
                .read_postings(&term_0, IndexRecordOption::Basic)
                .unwrap();

            for i in 0..num_docs / 2 - 1 {
                assert_eq!(segment_postings.skip_next(i * 2 + 1), SkipResult::OverStep);
                assert_eq!(segment_postings.doc(), (i + 1) * 2);
            }
        }

        // delete some of the documents
        {
            let mut index_writer = index.writer_with_num_threads(1, 40_000_000).unwrap();
            index_writer.delete_term(term_0);
            assert!(index_writer.commit().is_ok());
        }
        index.load_searchers().unwrap();
        let searcher = index.searcher();
        let segment_reader = searcher.segment_reader(0);

        // make sure seeking still works
        for i in 0..num_docs {
            let mut segment_postings = segment_reader
                .inverted_index(term_2.field())
                .read_postings(&term_2, IndexRecordOption::Basic)
                .unwrap();

            if i % 2 == 0 {
                assert_eq!(segment_postings.skip_next(i), SkipResult::Reached);
                assert_eq!(segment_postings.doc(), i);
                assert!(segment_reader.is_deleted(i));
            } else {
                assert_eq!(segment_postings.skip_next(i), SkipResult::Reached);
                assert_eq!(segment_postings.doc(), i);
            }
        }

        // now try with a longer sequence
        {
            let mut segment_postings = segment_reader
                .inverted_index(term_2.field())
                .read_postings(&term_2, IndexRecordOption::Basic)
                .unwrap();

            let mut last = 2; // start from 5 to avoid seeking to 3 twice
            let mut cur = 3;
            loop {
                match segment_postings.skip_next(cur) {
                    SkipResult::End => break,
                    SkipResult::Reached => assert_eq!(segment_postings.doc(), cur),
                    SkipResult::OverStep => assert_eq!(segment_postings.doc(), cur + 1),
                }

                let next = cur + last;
                last = cur;
                cur = next;
            }
            assert_eq!(cur, 377);
        }

        // delete everything else
        {
            let mut index_writer = index.writer_with_num_threads(1, 40_000_000).unwrap();
                index_writer.delete_term(term_1);

            assert!(index_writer.commit().is_ok());
        }
        index.load_searchers().unwrap();

        let searcher = index.searcher();
        let segment_reader = searcher.segment_reader(0);

        // finally, check that it's empty
        {
            let mut segment_postings = segment_reader
                .inverted_index(term_2.field())
                .read_postings(&term_2, IndexRecordOption::Basic)
                .unwrap();

            assert_eq!(segment_postings.skip_next(0), SkipResult::Reached);
            assert_eq!(segment_postings.doc(), 0);
            assert!(segment_reader.is_deleted(0));

            let mut segment_postings = segment_reader
                .inverted_index(term_2.field())
                .read_postings(&term_2, IndexRecordOption::Basic)
                .unwrap();

            assert_eq!(segment_postings.skip_next(num_docs), SkipResult::End);
        }
    }

    lazy_static! {
        static ref TERM_A: Term = {
            let field = Field(0);
            Term::from_field_text(field, "a")
        };
        static ref TERM_B: Term = {
            let field = Field(0);
            Term::from_field_text(field, "b")
        };
        static ref TERM_C: Term = {
            let field = Field(0);
            Term::from_field_text(field, "c")
        };
        static ref TERM_D: Term = {
            let field = Field(0);
            Term::from_field_text(field, "d")
        };
        static ref INDEX: Index = {
            let mut schema_builder = SchemaBuilder::default();
            let text_field = schema_builder.add_text_field("text", STRING);
            let schema = schema_builder.build();

            let seed: &[u32; 4] = &[1, 2, 3, 4];
            let mut rng: XorShiftRng = XorShiftRng::from_seed(*seed);

            let index = Index::create_in_ram(schema);
            let posting_list_size = 1_000_000;
            {
                let mut index_writer = index.writer_with_num_threads(1, 40_000_000).unwrap();
                for _ in 0 .. posting_list_size {
                    let mut doc = Document::default();
                    if rng.gen_weighted_bool(15) {
                        doc.add_text(text_field, "a");
                    }
                    if rng.gen_weighted_bool(10) {
                        doc.add_text(text_field, "b");
                    }
                    if rng.gen_weighted_bool(5) {
                        doc.add_text(text_field, "c");
                    }
                    if rng.gen_weighted_bool(1) {
                        doc.add_text(text_field, "d");
                    }
                    index_writer.add_document(doc);
                }
                assert!(index_writer.commit().is_ok());
            }
            index.load_searchers().unwrap();
            index
        };
    }

    #[bench]
    fn bench_segment_postings(b: &mut Bencher) {
        let searcher = INDEX.searcher();
        let segment_reader = searcher.segment_reader(0);

        b.iter(|| {
            let mut segment_postings = segment_reader
                .inverted_index(TERM_A.field())
                .read_postings(&*TERM_A, IndexRecordOption::Basic)
                .unwrap();
            while segment_postings.advance() {}
        });
    }

    #[bench]
    fn bench_segment_intersection(b: &mut Bencher) {
        let searcher = INDEX.searcher();
        let segment_reader = searcher.segment_reader(0);
        b.iter(|| {
            let segment_postings_a = segment_reader
                .inverted_index(TERM_A.field())
                .read_postings(&*TERM_A, IndexRecordOption::Basic)
                .unwrap();
            let segment_postings_b = segment_reader
                .inverted_index(TERM_B.field())
                .read_postings(&*TERM_B, IndexRecordOption::Basic)
                .unwrap();
            let segment_postings_c = segment_reader
                .inverted_index(TERM_C.field())
                .read_postings(&*TERM_C, IndexRecordOption::Basic)
                .unwrap();
            let segment_postings_d = segment_reader
                .inverted_index(TERM_D.field())
                .read_postings(&*TERM_D, IndexRecordOption::Basic)
                .unwrap();
            let mut intersection = Intersection::new(vec![
                segment_postings_a,
                segment_postings_b,
                segment_postings_c,
                segment_postings_d,
            ]);
            while intersection.advance() {}
        });
    }

    fn bench_skip_next(p: f32, b: &mut Bencher) {
        let searcher = INDEX.searcher();
        let segment_reader = searcher.segment_reader(0);
        let docs = tests::sample(segment_reader.num_docs(), p);

        let mut segment_postings = segment_reader
            .inverted_index(TERM_A.field())
            .read_postings(&*TERM_A, IndexRecordOption::Basic)
            .unwrap();

        let mut existing_docs = Vec::new();
        segment_postings.advance();
        for doc in &docs {
            if *doc >= segment_postings.doc() {
                existing_docs.push(*doc);
                if segment_postings.skip_next(*doc) == SkipResult::End {
                    break;
                }
            }
        }

        b.iter(|| {
            let mut segment_postings = segment_reader
                .inverted_index(TERM_A.field())
                .read_postings(&*TERM_A, IndexRecordOption::Basic)
                .unwrap();
            for doc in &existing_docs {
                if segment_postings.skip_next(*doc) == SkipResult::End {
                    break;
                }
            }
        });
    }

    #[bench]
    fn bench_skip_next_p01(b: &mut Bencher) {
        bench_skip_next(0.001, b);
    }

    #[bench]
    fn bench_skip_next_p1(b: &mut Bencher) {
        bench_skip_next(0.01, b);
    }

    #[bench]
    fn bench_skip_next_p10(b: &mut Bencher) {
        bench_skip_next(0.1, b);
    }

    #[bench]
    fn bench_skip_next_p90(b: &mut Bencher) {
        bench_skip_next(0.9, b);
    }

    #[bench]
    fn bench_iterate_segment_postings(b: &mut Bencher) {
        let searcher = INDEX.searcher();
        let segment_reader = searcher.segment_reader(0);
        b.iter(|| {
            let n: u32 = test::black_box(17);
            let mut segment_postings = segment_reader
                .inverted_index(TERM_A.field())
                .read_postings(&*TERM_A, IndexRecordOption::Basic)
                .unwrap();
            let mut s = 0u32;
            while segment_postings.advance() {
                s += (segment_postings.doc() & n) % 1024;
            }
            s
        });
    }

    /// Wraps a given docset, and forward alls call but the
    /// `.skip_next(...)`. This is useful to test that a specialized
    /// implementation of `.skip_next(...)` is consistent
    /// with the default implementation.
    pub(crate) struct UnoptimizedDocSet<TDocSet: DocSet>(TDocSet);

    impl<TDocSet: DocSet> UnoptimizedDocSet<TDocSet> {
        pub fn wrap(docset: TDocSet) -> UnoptimizedDocSet<TDocSet> {
            UnoptimizedDocSet(docset)
        }
    }

    impl<TDocSet: DocSet> DocSet for UnoptimizedDocSet<TDocSet> {
        fn advance(&mut self) -> bool {
            self.0.advance()
        }

        fn doc(&self) -> DocId {
            self.0.doc()
        }

        fn size_hint(&self) -> u32 {
            self.0.size_hint()
        }
    }

    impl<TScorer: Scorer> Scorer for UnoptimizedDocSet<TScorer> {
        fn score(&mut self) -> Score {
            self.0.score()
        }
    }

    pub fn test_skip_against_unoptimized<F: Fn() -> Box<DocSet>>(
        postings_factory: F,
        targets: Vec<u32>,
    ) {
        for target in targets {
            let mut postings_opt = postings_factory();
            let mut postings_unopt = UnoptimizedDocSet::wrap(postings_factory());
            let skip_result_opt = postings_opt.skip_next(target);
            let skip_result_unopt = postings_unopt.skip_next(target);
            assert_eq!(
                skip_result_unopt, skip_result_opt,
                "Failed while skipping to {}",
                target
            );
            match skip_result_opt {
                SkipResult::Reached => assert_eq!(postings_opt.doc(), target),
                SkipResult::OverStep => assert!(postings_opt.doc() > target),
                SkipResult::End => {
                    return;
                }
            }
            while postings_opt.advance() {
                assert!(postings_unopt.advance());
                assert_eq!(
                    postings_opt.doc(),
                    postings_unopt.doc(),
                    "Failed while skipping to {}",
                    target
                );
            }
            assert!(!postings_unopt.advance());
        }
    }

}
