/*
It contains the tokenizer for the indexer. The tokenizer is responsible for breaking down the text into
tokens that can be indexed and searched. This is a trait that can be implemented by different tokenizers,
such as a simple whitespace tokenizer or a more complex one that handles punctuation and special characters.
*/

pub trait Tokenizer {
    fn tokenize(&self, text: &str) -> Vec<String>;
}
