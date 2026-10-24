# Engineering notebook

## Fast retrieval

A block directory records the final document ID and byte offset of each compressed posting block. Galloping and binary search find the relevant block without decoding everything before it. The dictionary stays in memory while the compressed inverted index remains on disk.

## Why passages matter

A document can contain hundreds of unrelated paragraphs. Passage search returns the section that answers the query. Overlapping text windows preserve context at chunk boundaries, while file and page metadata point back to the source.

## Local by default

Indexify extracts PDF, Markdown, and DOCX content locally. Stored text snapshots make previews available even after an original document moves. Refreshing rebuilds a complete collection and replaces the previous snapshot only after a successful build.
