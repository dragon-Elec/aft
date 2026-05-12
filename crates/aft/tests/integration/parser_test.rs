use aft::parser::FileParser;

#[test]
fn python_decorated_function_range_includes_decorators() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let file = tmp.path().join("decorated.py");
    std::fs::write(&file, "@cache\n@profile\ndef f():\n    pass\n").expect("write python file");

    let mut parser = FileParser::new();
    let symbols = parser.extract_symbols(&file).expect("extract symbols");
    let symbol = symbols
        .iter()
        .find(|sym| sym.name == "f")
        .expect("find decorated function");

    assert_eq!(symbol.range.start_line, 0, "range should start at @cache");
    assert_eq!(symbol.range.start_col, 0);
}
