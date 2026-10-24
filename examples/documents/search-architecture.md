# Search architecture

## A small index, a useful answer

An inverted index maps each term to the passages that contain it. Instead of scanning every document for every query, the search engine follows a compact posting list. Document gaps and term frequencies can be compressed into independently readable blocks.

## Ranking the right passage

BM25 ranks passages using term rarity, term frequency, and passage length. A word appearing in a few documents carries more information than one appearing almost everywhere. Repeated words help, but the benefit saturates. Length normalization keeps a long document from winning simply because it contains more text.

## Keeping memory under control

External sorting builds an inverted index in bounded batches. Each batch becomes a sorted run on disk. A multiway merge combines those runs and streams postings directly into the compression writer. This avoids retaining the entire corpus in memory during index construction.

## Finding the original

Every passage keeps its source filename and location. Markdown uses line numbers, PDFs use pages, and Word documents use paragraph numbers. Search results should always include enough context to find the original document.
