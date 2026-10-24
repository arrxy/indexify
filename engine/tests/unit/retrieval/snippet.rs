use super::*;
#[test]
fn selects_query_region_and_preserves_unicode() {
    let text = format!(
        "{} Café naïve 東京 rust search. {}",
        "unrelated ".repeat(100),
        "trailing ".repeat(100)
    );
    let terms = vec!["café".into(), "東京".into(), "rust".into()];
    let s = generate(&text, &terms);
    assert!(s.contains("[Café]"));
    assert!(s.contains("[東京]"));
    assert!(s.contains("[rust]"));
    assert!(s.starts_with("..."));
    assert!(s.chars().count() < 270);
}
#[test]
fn covers_separated_query_terms_in_one_window() {
    let text = format!(
        "{}rust {} search{}",
        "x ".repeat(200),
        "x ".repeat(100),
        " x".repeat(200)
    );
    let snippet = generate(&text, &["rust".into(), "search".into()]);
    assert!(snippet.contains("[rust]"));
    assert!(snippet.contains("[search]"));
}
#[test]
fn matches_whole_normalized_tokens() {
    assert_eq!(
        generate("RUST rusty rust!", &["rust".into()]),
        "[RUST] rusty [rust]!"
    );
    assert_eq!(generate("", &[]), "");
}
