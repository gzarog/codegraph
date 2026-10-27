//! JSX renders: `<ItemCard item={it} />` inside `ItemGrid` means ItemGrid
//! renders (calls) ItemCard. The extraction kernel records no reference for
//! an element's tag, so these come from here.

use crate::util::Lines;
use codegraph_core::Language;
use regex::Regex;
use std::sync::LazyLock;

static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<([A-Z][A-Za-z0-9_$]*(?:\.[A-Z][A-Za-z0-9_$]*)*)[\s/>]").unwrap()
});

/// Languages whose files may contain JSX.
pub fn applies(lang: Language) -> bool {
    matches!(lang, Language::Tsx | Language::Jsx | Language::JavaScript)
}

/// `(line, component)` for each capitalized element in `safe` (comments
/// stripped). A `<` right after an identifier is a type argument, not JSX.
pub fn renders(safe: &str) -> Vec<(u32, String)> {
    let lines = Lines::new(safe);
    let mut out = Vec::new();
    for c in TAG.captures_iter(safe) {
        let at = c.get(0).unwrap().start();
        let before = safe[..at].trim_end();
        let jsx = match before.chars().last() {
            None => true,
            Some(ch) => {
                "({}>=?:,&|[;".contains(ch) || before.ends_with("return") || before.ends_with("=>")
            }
        };
        if jsx {
            out.push((lines.line(at), c[1].to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_elements_not_generics() {
        let src = "const a: Array<Item> = [];\nfunction G() {\n  return <div>{xs.map((x) => <Card item={x} />)}<Ui.Button/></div>;\n}\nconst b = useState<Foo>(null);\n";
        let r = renders(src);
        assert_eq!(
            r,
            vec![(3, "Card".to_string()), (3, "Ui.Button".to_string())]
        );
    }
}
