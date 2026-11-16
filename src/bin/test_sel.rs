fn main() {
    let html = std::fs::read_to_string("ddg.html").unwrap();
    let document = scraper::Html::parse_document(&html);
    
    let selectors = [
        "div.result__body",
        "div",
        "a.result__a", 
        ".result__body",
        "div[class]",
    ];
    
    for sel_str in &selectors {
        let sel = scraper::Selector::parse(sel_str).unwrap();
        let count = document.select(&sel).count();
        println!("  {} -> {} matches", sel_str, count);
    }
}
