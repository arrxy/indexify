/* Exact document-at-a-time retrieval over compressed posting cursors.
AND aligns cursors (rarest term first); OR visits the union in doc-ID order.
Only ten hits and one decoded block per term are retained. Equal scores
are ordered by internal doc ID for deterministic results. */
use super::{bm25::Bm25, snippet};
use crate::compression::Simple9;
use crate::index::{CollectionStats, Lexicon, PageTable, PassageStore, PostingsReader};
use crate::tokenizer::{SimpleTokenizer, Tokenizer};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    Conjunctive,
    Disjunctive,
}
impl QueryMode {
    pub fn parse(value: &str) -> io::Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "and" | "conjunctive" => Ok(Self::Conjunctive),
            "or" | "disjunctive" => Ok(Self::Disjunctive),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Mode must be 'and' or 'or'",
            )),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Conjunctive => "AND",
            Self::Disjunctive => "OR",
        }
    }
}

#[derive(Debug)]
pub struct TermScore {
    pub term: String,
    pub frequency: u32,
    pub score: f64,
}
#[derive(Debug)]
pub struct SearchHit {
    pub doc_id: u32,
    pub passage_id: u32,
    pub score: f64,
    pub term_scores: Vec<TermScore>,
    pub snippet: String,
}
// BinaryHeap's root is the WORST retained result.
impl PartialEq for SearchHit {
    fn eq(&self, other: &Self) -> bool {
        self.doc_id == other.doc_id && self.score.total_cmp(&other.score) == Ordering::Equal
    }
}
impl Eq for SearchHit {}
impl Ord for SearchHit {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then(self.doc_id.cmp(&other.doc_id))
    }
}
impl PartialOrd for SearchHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct SearchResult {
    pub terms: Vec<String>,
    pub hits: Vec<SearchHit>,
    pub matched_documents: u64,
}
struct TermCursor {
    term_index: usize,
    reader: PostingsReader<Simple9>,
    current: Option<u32>,
}

pub struct SearchEngine {
    index_dir: PathBuf,
    lexicon: Lexicon,
    page_table: PageTable,
    stats: CollectionStats,
    bm25: Bm25,
    passages: Option<PassageStore>,
}
impl SearchEngine {
    pub fn open(
        index_dir: impl AsRef<Path>,
        dataset: Option<&Path>,
        snippets: bool,
        k1: f64,
        b: f64,
    ) -> io::Result<Self> {
        let index_dir = index_dir.as_ref().to_owned();
        let stats = CollectionStats::load(index_dir.join("metadata.bin"))?;
        if stats.tokenizer_id != 1 || stats.tokenizer_version != 1 {
            return Err(invalid(
                "Unsupported index tokenizer; query normalization must match indexing",
            ));
        }
        let page_table = PageTable::load(index_dir.join("page_table.bin"))?;
        if page_table.entries.len() as u64 != stats.document_count {
            return Err(invalid(
                "Page-table document count differs from collection metadata",
            ));
        }
        let total: u64 = page_table
            .entries
            .iter()
            .map(|e| u64::from(e.doc_length))
            .sum();
        if total != stats.total_doc_length {
            return Err(invalid(
                "Page-table token count differs from collection metadata",
            ));
        }
        let lexicon = Lexicon::load(index_dir.join("lexicon.bin"))?;
        // Validate even an empty index before entering the query loop.
        for (name, magic) in [("postings.bin", b"IDX1"), ("blocks.bin", b"BLK1")] {
            use std::io::Read;
            let mut header = [0; 4];
            std::fs::File::open(index_dir.join(name))?.read_exact(&mut header)?;
            if &header != magic {
                return Err(invalid("Unsupported postings or block-directory header"));
            }
        }
        if lexicon
            .terms
            .values()
            .any(|e| u64::from(e.doc_freq) > stats.document_count)
        {
            return Err(invalid(
                "Term document frequency exceeds collection document count",
            ));
        }
        let passages = if snippets {
            Some(PassageStore::open(&page_table, dataset)?)
        } else {
            None
        };
        let bm25 = Bm25::new(&stats, k1, b)?;
        Ok(Self {
            index_dir,
            lexicon,
            page_table,
            stats,
            bm25,
            passages,
        })
    }

    pub fn document_count(&self) -> u64 {
        self.stats.document_count
    }
    pub fn term_count(&self) -> usize {
        self.lexicon.terms.len()
    }

    /// Read the immutable indexed passage for the application preview pane.
    pub fn passage_text(&mut self, doc_id: u32) -> io::Result<String> {
        let entry = self
            .page_table
            .entries
            .get(doc_id as usize)
            .ok_or_else(|| invalid("Passage ID outside the page table"))?;
        self.passages
            .as_mut()
            .ok_or_else(|| invalid("Passage text is not enabled"))?
            .text(entry)
    }

    pub fn search(&mut self, query: &str, mode: QueryMode) -> io::Result<SearchResult> {
        let mut seen = HashSet::new();
        let terms: Vec<String> = SimpleTokenizer
            .tokenize(query)
            .into_iter()
            .filter(|t| seen.insert(t.clone()))
            .collect();
        if terms.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Queries may contain at most 64 distinct terms",
            ));
        }
        let mut result = SearchResult {
            terms,
            hits: Vec::new(),
            matched_documents: 0,
        };
        if result.terms.is_empty() {
            return Ok(result);
        }
        let mut cursors = Vec::new();
        let mut idfs = vec![0.0; result.terms.len()];
        for (term_index, term) in result.terms.iter().enumerate() {
            let Some(entry) = self.lexicon.terms.get(term) else {
                if mode == QueryMode::Conjunctive {
                    return Ok(result);
                }
                continue;
            };
            idfs[term_index] = self.bm25.idf(entry.doc_freq)?;
            let reader = PostingsReader::open(&self.index_dir, entry, Simple9)?;
            cursors.push(TermCursor {
                term_index,
                reader,
                current: None,
            });
        }
        if cursors.is_empty() {
            return Ok(result);
        }
        let mut heap = BinaryHeap::with_capacity(11);
        let mut frequencies = vec![0; result.terms.len()];
        if mode == QueryMode::Conjunctive {
            cursors.sort_by_key(|c| c.reader.doc_freq());
            let mut target = 0;
            'align: loop {
                for cursor in &mut cursors {
                    let Some(doc_id) = cursor.reader.next_geq(target)? else {
                        break 'align;
                    };
                    if doc_id > target {
                        target = doc_id;
                        continue 'align;
                    }
                }
                frequencies.fill(0);
                for cursor in &cursors {
                    frequencies[cursor.term_index] = cursor
                        .reader
                        .frequency()
                        .ok_or_else(|| invalid("Posting cursor has no frequency"))?;
                }
                self.consider(target, &result.terms, &idfs, &frequencies, &mut heap)?;
                result.matched_documents += 1;
                let Some(next) = target.checked_add(1) else {
                    break;
                };
                target = next;
            }
        } else {
            for cursor in &mut cursors {
                cursor.current = cursor.reader.next_geq(0)?;
            }
            while let Some(doc_id) = cursors.iter().filter_map(|c| c.current).min() {
                frequencies.fill(0);
                for cursor in &cursors {
                    if cursor.current == Some(doc_id) {
                        frequencies[cursor.term_index] = cursor
                            .reader
                            .frequency()
                            .ok_or_else(|| invalid("Posting cursor has no frequency"))?;
                    }
                }
                self.consider(doc_id, &result.terms, &idfs, &frequencies, &mut heap)?;
                result.matched_documents += 1;
                for cursor in &mut cursors {
                    if cursor.current == Some(doc_id) {
                        cursor.current = match doc_id.checked_add(1) {
                            Some(next) => cursor.reader.next_geq(next)?,
                            None => None,
                        };
                    }
                }
            }
        }
        result.hits = heap.into_vec();
        result
            .hits
            .sort_by(|a, b| b.score.total_cmp(&a.score).then(a.doc_id.cmp(&b.doc_id)));
        if let Some(passages) = &mut self.passages {
            for hit in &mut result.hits {
                let entry = self
                    .page_table
                    .get(hit.doc_id)
                    .ok_or_else(|| invalid("Result document is outside page table"))?;
                hit.snippet = snippet::generate(&passages.text(entry)?, &result.terms);
            }
        }
        Ok(result)
    }

    fn consider(
        &self,
        doc_id: u32,
        terms: &[String],
        idfs: &[f64],
        frequencies: &[u32],
        heap: &mut BinaryHeap<SearchHit>,
    ) -> io::Result<()> {
        let entry = self
            .page_table
            .get(doc_id)
            .ok_or_else(|| invalid("Posting document ID is outside page table"))?;
        if frequencies.iter().any(|&tf| tf > entry.doc_length) {
            return Err(invalid("Term frequency exceeds document length"));
        }
        let score: f64 = frequencies
            .iter()
            .zip(idfs)
            .map(|(&tf, &idf)| self.bm25.score(tf, entry.doc_length, idf))
            .sum();
        if heap.len() == 10
            && heap.peek().is_some_and(|worst| {
                score < worst.score || (score == worst.score && doc_id >= worst.doc_id)
            })
        {
            return Ok(());
        }
        let term_scores = terms
            .iter()
            .zip(frequencies)
            .zip(idfs)
            .map(|((term, &frequency), &idf)| TermScore {
                term: term.clone(),
                frequency,
                score: self.bm25.score(frequency, entry.doc_length, idf),
            })
            .collect();
        heap.push(SearchHit {
            doc_id,
            passage_id: entry.passage_id,
            score,
            term_scores,
            snippet: String::new(),
        });
        if heap.len() > 10 {
            heap.pop();
        }
        Ok(())
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
