use std::collections::BTreeSet;
use std::io;

use common::file_slice::FileSlice;
use criterion::{Criterion, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tantivy_fst::Automaton;
use tantivy_sstable::{Dictionary, MonotonicU64SSTable};

const CHARSET: &[u8] = b"abcdefghij";

// This intentionally only supports needles without repeated bytes so we don't need KMP style
// handling.
struct SimpleSubsliceAutomaton<'a> {
    needle: &'a [u8],
}

impl<'a> SimpleSubsliceAutomaton<'a> {
    fn new(needle: &'a [u8]) -> Self {
        debug_assert!(!needle.is_empty());
        debug_assert!(
            needle
                .iter()
                .enumerate()
                .all(|(i, byte)| !needle[..i].contains(byte))
        );
        Self { needle }
    }
}

impl Automaton for SimpleSubsliceAutomaton<'_> {
    type State = usize;

    fn start(&self) -> Self::State {
        0
    }

    fn is_match(&self, state: &Self::State) -> bool {
        *state == self.needle.len()
    }

    fn will_always_match(&self, state: &Self::State) -> bool {
        self.is_match(state)
    }

    fn accept(&self, state: &Self::State, byte: u8) -> Self::State {
        if self.is_match(state) {
            return *state;
        }
        if byte == self.needle[*state] {
            *state + 1
        } else {
            (byte == self.needle[0]).into()
        }
    }
}

fn generate_key(rng: &mut impl Rng) -> String {
    let len = rng.random_range(3..12);
    std::iter::from_fn(|| {
        let idx = rng.random_range(0..CHARSET.len());
        Some(CHARSET[idx] as char)
    })
    .take(len)
    .collect()
}

fn prepare_sstable() -> io::Result<Dictionary<MonotonicU64SSTable>> {
    let mut rng = StdRng::from_seed([3u8; 32]);
    let mut els = BTreeSet::new();
    while els.len() < 100_000 {
        els.insert(generate_key(&mut rng));
    }
    let mut dictionary_builder = Dictionary::<MonotonicU64SSTable>::builder(Vec::new())?;
    for (ord, word) in els.iter().enumerate() {
        dictionary_builder.insert(word, &(ord as u64))?;
    }
    let buffer = dictionary_builder.finish()?;
    let dictionary = Dictionary::open(FileSlice::from(buffer))?;
    Ok(dictionary)
}

fn stream_bench(
    dictionary: &Dictionary<MonotonicU64SSTable>,
    lower: &[u8],
    upper: &[u8],
    do_scan: bool,
) -> usize {
    let mut stream = dictionary
        .range()
        .ge(lower)
        .lt(upper)
        .into_stream()
        .unwrap();
    if !do_scan {
        return 0;
    }
    let mut count = 0;
    while stream.advance() {
        count += 1;
    }
    count
}

pub fn criterion_benchmark(c: &mut Criterion) {
    let dict = prepare_sstable().unwrap();
    c.bench_function("short_scan_init", |b| {
        b.iter(|| stream_bench(&dict, b"fa", b"fana", false))
    });
    c.bench_function("short_scan_init_and_scan", |b| {
        b.iter(|| {
            assert_eq!(stream_bench(&dict, b"fa", b"faz", true), 1051);
        })
    });
    c.bench_function("full_scan_init_and_scan_full_with_bound", |b| {
        b.iter(|| {
            assert_eq!(stream_bench(&dict, b"", b"z", true), 100_000);
        })
    });
    c.bench_function("full_scan_init_and_scan_full_no_bounds", |b| {
        b.iter(|| {
            let mut stream = dict.stream().unwrap();
            let mut count = 0;
            while stream.advance() {
                count += 1;
            }
            count
        })
    });
    c.bench_function("full_scan_simple_subslice_automaton", |b| {
        b.iter(|| {
            let mut stream = dict
                .search(SimpleSubsliceAutomaton::new(b"abcef"))
                .into_stream()
                .unwrap();
            let mut count = 0;
            while stream.advance() {
                count += 1;
            }
            count
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
