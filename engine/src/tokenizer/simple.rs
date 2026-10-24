use crate::tokenizer::tokenizer::Tokenizer;
pub struct SimpleTokenizer;

impl Tokenizer for SimpleTokenizer {
    fn tokenize(&self, text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|term| !term.is_empty())
            .map(|term| term.to_lowercase())
            .collect()
    }
}
