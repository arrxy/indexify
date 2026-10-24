This is a CPU-only source snapshot of the existing search-engine project at
`/Users/aritro/Aritro/rustlab/search-engine`, copied on 23 October 2026.

It preserves the external-sort indexer, compressed postings, block directories,
BM25 ranking, AND/OR matching, and original tests. Indexify calls its library
directly. The only retrieval addition is `SearchEngine::passage_text`, which
provides the original indexed passage for the preview pane. The copied legacy
CLI also rejects invalid sort options and out-of-range numeric settings before
creating output directories. Its error-path tests use isolated temporary output
paths. An unused
historical index wrapper is annotated to avoid dead-code warnings.

The upstream checkout is not modified or required at runtime. GPU dependencies
are omitted from this standalone application.
