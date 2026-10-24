use super::*;

#[test]
fn term_ids_preserve_utf8_lexicographic_order() {
    let postings = vec![
        IntermediatePosting {
            term: "é".into(),
            doc_id: 7,
            term_frequency: 1,
        },
        IntermediatePosting {
            term: "z".into(),
            doc_id: u32::MAX,
            term_frequency: 2,
        },
        IntermediatePosting {
            term: "é".into(),
            doc_id: 0,
            term_frequency: 3,
        },
        IntermediatePosting {
            term: "a".into(),
            doc_id: 8,
            term_frequency: 4,
        },
    ];
    let records = numeric_keys(&postings).unwrap();
    assert_eq!(records, vec![[2, 7], [1, u32::MAX], [2, 0], [0, 8]]);
}

#[test]
#[ignore = "Requires a hardware Metal GPU; run with --features gpu-sort -- --ignored"]
fn gpu_radix_matches_cpu_oracle_at_block_boundaries_and_full_u32_keys() {
    let radix = RadixSorter::new().unwrap();
    for count in [0, 1, 2, 15, 255, 256, 257, 1025, 65537] {
        let mut state = 0x1234_5678_u32;
        let mut records: Vec<[u32; 2]> = (0..count)
            .map(|index| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                let term = if index % 7 == 0 { u32::MAX } else { state % 33 };
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                [term, if index % 13 == 0 { 0 } else { state }]
            })
            .collect();
        for shape in 0..3 {
            if shape == 1 {
                for record in &mut records {
                    record[0] = 0;
                    record[1] = 0;
                }
            } else if shape == 2 {
                for (i, record) in records.iter_mut().enumerate() {
                    record[0] = (count as usize - i) as u32;
                    record[1] = u32::MAX;
                }
            }
            let mut expected: Vec<_> = (0..records.len()).collect();
            expected.sort_by_key(|&i| (records[i][0], records[i][1]));
            let indices = radix.sort_indices(&records).unwrap();
            assert_eq!(
                indices
                    .iter()
                    .map(|&index| index as usize)
                    .collect::<Vec<_>>(),
                expected,
                "count={count}, shape={shape}"
            );
        }
    }
    let sorter = GpuSorter::new().unwrap();
    let mut postings: Vec<_> = (0..1031)
        .rev()
        .map(|i| IntermediatePosting {
            term: ["rust", "é", "a", "z", "東京"][i % 5].into(),
            doc_id: i as u32,
            term_frequency: i as u32 % 11 + 1,
        })
        .collect();
    let mut expected = postings.clone();
    expected.sort_by(|a, b| a.term.cmp(&b.term).then(a.doc_id.cmp(&b.doc_id)));
    let original = postings.clone();
    let PostingOrder::Indices(indices) = sorter.sort(&mut postings).unwrap() else {
        panic!("GPU must return sorted indices");
    };
    for (a, b) in indices.iter().map(|&i| &postings[i as usize]).zip(expected) {
        assert_eq!(
            (&a.term, a.doc_id, a.term_frequency),
            (&b.term, b.doc_id, b.term_frequency)
        );
    }
    for (a, b) in postings.iter().zip(&original) {
        assert_eq!(
            (&a.term, a.doc_id, a.term_frequency),
            (&b.term, b.doc_id, b.term_frequency)
        );
    }
    assert!(
        matches!(sorter.sort(&mut []).unwrap(), PostingOrder::Indices(indices) if indices.is_empty())
    );
}
