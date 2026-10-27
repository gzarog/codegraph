//! `codegraph prompt-hook`: a Claude Code `UserPromptSubmit` hook. For a
//! structural prompt ("how does X reach Y", or one naming code identifiers
//! the index knows) it front-loads `explore` output; for prose whose words
//! match indexed symbol names it lists those symbols. Anything else is a
//! silent no-op.

use crate::project::{find_root, CodeGraph};
use codegraph_core::search::{extract_prose_candidates, segment_lookup_variants};
use codegraph_core::NodeKind;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

const NOT_WORD_BEFORE: &str = r"(?:^|[^\p{L}\p{N}_])";
const NOT_WORD_AFTER: &str = r"(?:$|[^\p{L}\p{N}_])";

const STRUCTURAL_WORDS: &[&str] = &[
    "how",
    "where",
    "tracing",
    "flows?",
    "paths?",
    "reach(?:es|ed)?",
    "wired?",
    "breaks?",
    "why does",
    "comment",
    "où",
    "flux",
    "chemins?",
    "casse",
    "cómo",
    "dónde",
    "donde",
    "flujos?",
    "rutas?",
    "caminos?",
    "rompe",
    "llaman",
    "quién llama",
    "quien llama",
    "onde",
    "fluxos?",
    "caminhos?",
    "wo",
    "woher",
    "wohin",
    "pfade?",
    "fluss",
    "ablauf",
    "bricht",
    "kaputt",
    "ruft",
    "hängt",
    "dove",
    "flusso",
    "percors[oi]",
    "как",
    "где",
    "путь",
    "пути",
    "работает",
    "як",
    "де",
    "потік",
    "працює",
    "hoe",
    "waar",
    "roept",
    "werkt",
    "aangeroepen",
    "jak",
    "gdzie",
    "kde",
    "cesta",
    "cum",
    "unde",
    "hogyan",
    "hol",
    "nasıl",
    "mimari",
    "takip",
    "bagaimana",
    "di mana",
    "dimana",
    "alur",
    "jalur",
    "làm sao",
    "thế nào",
    "ở đâu",
    "gọi",
    "phụ thuộc",
    "ảnh hưởng",
    "kiến trúc",
    "cấu trúc",
    "luồng",
    "đường dẫn",
    "hoạt động",
    "giải thích",
    "theo dõi",
    "hur",
    "hvordan",
    "hvor",
    "beror",
    "flöde",
    "miten",
    "missä",
    "toimii",
    "πώς",
    "πού",
    "καλεί",
    "δομή",
    "ροή",
    "कैसे",
    "कहाँ",
    "कहां",
    "कॉल",
    "निर्भर",
    "संरचना",
    "प्रवाह",
];

const STRUCTURAL_STEMS: &[&str] = &[
    "architect",
    "structur",
    "depend",
    "implement",
    "impact",
    "explain",
    r"call(?:s|ing|ed|ers?|backs?|able|sites?)?(?:$|[^\p{L}\p{N}_])",
    r"trace(?:s|d|rs?)?(?:$|[^\p{L}\p{N}_])",
    r"affect(?:s|ed|ing)?(?:$|[^\p{L}\p{N}_])",
    r"connect(?:s|ed|ing|ions?|ors?|ivity)?(?:$|[^\p{L}\p{N}_])",
    "appel",
    "dépend",
    "implément",
    "connex",
    "expliqu",
    "fonctionn",
    "llamad",
    "afect",
    "conect",
    "conexi",
    "arquitec",
    "estructur",
    "funcion",
    "traza",
    "explica",
    "chama",
    "afeta",
    "arquitet",
    "estrutur",
    "quebra",
    "abhäng",
    "auswirkung",
    "beeinfluss",
    "verbind",
    "architekt",
    "struktur",
    "funktionier",
    "aufruf",
    "aufgerufen",
    "erklär",
    "verfolg",
    "chiam",
    "dipend",
    "impatt",
    "connett",
    "conness",
    "architett",
    "struttur",
    "funzion",
    "tracci",
    "spiega",
    "вызыва",
    "завис",
    "влия",
    "реализ",
    "структур",
    "архитектур",
    "трассир",
    "лома",
    "объясн",
    "поток",
    "виклика",
    "залеж",
    "вплива",
    "архітектур",
    "реаліз",
    "поясн",
    "шлях",
    "aanroep",
    "afhankelijk",
    "beïnvloed",
    "structuur",
    "uitleg",
    "wywoł",
    "zależ",
    "wpływ",
    "przepływ",
    "ścieżk",
    "działa",
    "wyjaśni",
    "śledz",
    "volá",
    "závis",
    "ovlivň",
    "funguj",
    "vysvětl",
    "apel",
    "depind",
    "arhitectur",
    "funcțion",
    "explică",
    "hív",
    "függ",
    "működ",
    "struktúr",
    "magyaráz",
    "nere",
    "çağır",
    "çağrı",
    "bağıml",
    "bağlant",
    "akış",
    "etkile",
    "etkisi",
    "panggil",
    "memanggil",
    "dipanggil",
    "bergantung",
    "tergantung",
    "pengaruh",
    "mempengaruhi",
    "memengaruhi",
    "arsitektur",
    "fungsi",
    "berfungsi",
    "jelaskan",
    "menjelaskan",
    "anrop",
    "påverk",
    "påvirk",
    "afhæng",
    "avheng",
    "förklar",
    "forklar",
    "arkitektur",
    "funger",
    "kutsu",
    "riippu",
    "arkkitehtuur",
    "rakente",
    "selit",
    "εξαρτ",
    "επηρε",
    "αρχιτεκτονικ",
    "διαδρομ",
    "εξηγ",
    "εξήγ",
    "समझा",
    "आर्किटेक्चर",
];

const STRUCTURAL_UNSEGMENTED: &str = "如何|怎么|怎麼|在哪|哪里|哪裡|追踪|跟踪|追蹤|追跡|トレース|流程|流向|流れ|路径|路徑|経路|调用|調用|呼び出|依赖|依賴|依存|影响|影響|实现|實現|実装|架构|架構|アーキテクチャ|结构|結構|構造|介绍|介紹|解析|分析|原理|机制|機制|仕組み|説明|動作|どうやって|どのように|어떻게|어디|호출|흐름|경로|의존|영향|구현|구조|아키텍처|추적|동작|작동|설명|كيف|أين|اين|يستدعي|استدعاء|يعتمد|تعتمد|يؤثر|تأثير|معماري|بنية|هيكل|تدفق|مسار|تتبع|يعمل|تعمل|اشرح|شرح|چگونه|چطور|کجا|فراخوان|وابسته|تأثیر|معماری|ساختار|مسیر|توضیح|איך|איפה|קורא|תלוי|משפיע|ארכיטקטור|מבנה|זרימה|נתיב|הסבר|อย่างไร|ยังไง|ที่ไหน|เรียกใช้|ขึ้นอยู่กับ|ผลกระทบ|สถาปัตยกรรม|โครงสร้าง|เส้นทาง|ติดตาม|ทำงาน|อธิบาย";

static WORDS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        "(?iu){NOT_WORD_BEFORE}(?:{}){NOT_WORD_AFTER}",
        STRUCTURAL_WORDS.join("|")
    ))
    .unwrap()
});
static STEMS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        "(?iu){NOT_WORD_BEFORE}(?:{})",
        STRUCTURAL_STEMS.join("|")
    ))
    .unwrap()
});
static UNSEGMENTED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(STRUCTURAL_UNSEGMENTED).unwrap());
static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z_$][\w$]*").unwrap());
static CALL_FORM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([A-Za-z_$][\w$]*)\(").unwrap());
static MEMBER_FORM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Za-z_$][\w$]*)\.([A-Za-z_$][\w$]*)").unwrap());
static DOC_DATA_EXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(md|markdown|txt|rst|json|ya?ml|toml|lock|csv|tsv|log|ini|cfg|conf|env|xml|html?|png|jpe?g|gif|svg|pdf)$").unwrap()
});
static SYSTEM_REMINDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<system-reminder>.*?</system-reminder>").unwrap());

/// Output larger than this is cut, pointing the agent at `explore` for the rest.
pub const INJECTION_MAX: usize = 12_000;

/// A "how / where / why / calls / depends" question, in any covered language.
pub fn has_structural_keyword(prompt: &str) -> bool {
    !prompt.is_empty()
        && (WORDS_RE.is_match(prompt)
            || STEMS_RE.is_match(prompt)
            || UNSEGMENTED_RE.is_match(prompt))
}

/// Identifier-shaped tokens: camelCase, snake_case, `call(` and `a.b` forms.
pub fn extract_code_tokens(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };
    for m in IDENT.find_iter(prompt) {
        let w = m.as_str();
        let b = w.as_bytes();
        let camel = b
            .windows(2)
            .any(|p| p[0].is_ascii_lowercase() && p[1].is_ascii_uppercase());
        let snake = b
            .windows(3)
            .any(|p| p[0].is_ascii_alphanumeric() && p[1] == b'_' && p[2].is_ascii_alphanumeric());
        if camel || snake {
            add(w);
        }
    }
    for c in CALL_FORM.captures_iter(prompt) {
        add(&c[1]);
    }
    for c in MEMBER_FORM.captures_iter(prompt) {
        if !DOC_DATA_EXT.is_match(&c[0]) {
            add(&c[1]);
            add(&c[2]);
        }
    }
    out
}

pub fn strip_system_reminders(prompt: &str) -> String {
    SYSTEM_REMINDER.replace_all(prompt, "\n").trim().to_string()
}

/// Harness envelopes that aren't the user's own words.
fn is_envelope(prompt: &str) -> bool {
    let t = prompt.trim();
    (t.starts_with("<task-notification>") && t.ends_with("</task-notification>"))
        || (t.starts_with("<agent-message") && t.ends_with("</agent-message>"))
}

fn cap(text: &str) -> String {
    if text.len() <= INJECTION_MAX {
        return text.to_string();
    }
    let mut end = INJECTION_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n…(truncated; call codegraph_explore for the rest)",
        &text[..end]
    )
}

/// What the hook prints for `prompt` asked in `cwd`, if anything.
pub fn run(prompt: &str, cwd: &Path) -> Option<String> {
    let prompt = strip_system_reminders(prompt);
    if prompt.is_empty() || is_envelope(&prompt) {
        return None;
    }
    let keyworded = has_structural_keyword(&prompt);
    let tokens = if keyworded {
        Vec::new()
    } else {
        extract_code_tokens(&prompt)
    };
    let prose = if keyworded {
        Vec::new()
    } else {
        extract_prose_candidates(&prompt)
    };
    if !keyworded && tokens.is_empty() && prose.is_empty() {
        return None;
    }
    let root = find_root(cwd)?;
    let cg = CodeGraph::open(&root).ok()?;
    let token_known = tokens.iter().any(|t| {
        cg.store()
            .nodes_named(t)
            .map(|n| !n.is_empty())
            .unwrap_or(false)
    });
    if keyworded || token_known {
        let text = cg.explore(&prompt, None).ok()?;
        if text.trim().is_empty() || text.starts_with("No indexed symbols match") {
            return None;
        }
        return Some(format!(
            "<codegraph_context note=\"Structural context from CodeGraph for this prompt — treat returned source as already read; call codegraph_explore for more.\">\n{}\n</codegraph_context>\n",
            cap(&text)
        ));
    }
    // Prose: words that name parts of indexed symbols.
    let mut hits = Vec::new();
    for word in &prose {
        for v in segment_lookup_variants(word) {
            for name in cg.store().names_for_segment(&v, 5).unwrap_or_default() {
                for n in cg.store().nodes_named(&name).unwrap_or_default() {
                    if !matches!(
                        n.kind,
                        NodeKind::File
                            | NodeKind::Import
                            | NodeKind::Parameter
                            | NodeKind::Variable
                    ) && !hits.iter().any(|h: &codegraph_core::Node| h.id == n.id)
                    {
                        hits.push(n);
                    }
                }
            }
        }
        if hits.len() >= 12 {
            break;
        }
    }
    if hits.is_empty() {
        return None;
    }
    hits.truncate(12);
    let lines: Vec<String> = hits
        .iter()
        .map(|n| {
            format!(
                "  - {} ({} — {}:{})",
                n.name, n.kind, n.file_path, n.start_line
            )
        })
        .collect();
    let example: Vec<&str> = hits.iter().take(3).map(|n| n.name.as_str()).collect();
    Some(format!(
        "<codegraph_context note=\"CodeGraph found indexed symbols matching this prompt — query the graph before searching files.\">\nThis project's CodeGraph index contains symbols matching this request:\n{}\nCall codegraph_explore ONCE with the relevant names in one query (e.g. \"{}\") to get their source, call paths, and blast radius — cheaper and more complete than Read/Grep.\n</codegraph_context>\n",
        lines.join("\n"),
        example.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_structural_prompts() {
        assert!(has_structural_keyword(
            "How does login reach the session store?"
        ));
        assert!(has_structural_keyword("what calls parseConfig"));
        assert!(has_structural_keyword("¿Dónde se valida el token?"));
        assert!(has_structural_keyword("这个函数是如何工作的"));
        assert!(!has_structural_keyword("fix this typo please"));
        assert!(!has_structural_keyword("recalls the value")); // stem `call` needs a word start
        assert_eq!(
            extract_code_tokens("make getUserId use user.profile and parse() not README.md"),
            vec!["getUserId", "parse", "user", "profile"]
        );
        assert_eq!(
            strip_system_reminders("hi <system-reminder>x</system-reminder> there"),
            "hi \n there"
        );
        assert!(is_envelope("<task-notification>x</task-notification>"));
    }
}
